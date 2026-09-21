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

/// Marks bounded explicit GPT-5.6+ cache checkpoints in the durable prefix.
///
/// OpenAI permits up to four explicit writes per request. The first stable
/// `input_text` checkpoint preserves the original stationary boundary, while
/// the three newest stable checkpoints advance with durable chronology. A later
/// request can therefore read the most recent completed conversation prefix and
/// write the newly appended prefix without marking the volatile request suffix.
/// The first checkpoint and rolling tail remain within OpenAI's explicit lookup
/// boundaries, and the limit avoids cache-write amplification.
///
/// A stable non-developer block remains eligible: the wire role does not encode
/// placement, and requiring a developer block would reject otherwise valid
/// sessions whose only developer-rendered messages are volatile.
pub(crate) fn apply_openai_prompt_cache_breakpoint(
    request: &ModelRequest,
    stable_input_positions: &[usize],
    settled_history_cache_checkpoint_positions: &[usize],
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
    let targets = input.stable_input_positions_marker_targets(
        stable_input_positions,
        settled_history_cache_checkpoint_positions,
    );
    if targets.is_empty() {
        return Err(ProviderRequestAssemblyError::invalid_args(
            "OpenAI explicit prompt-cache mode requires a stable-prefix input_text content block",
        ));
    }
    for (message_position, block_index) in targets {
        input[message_position]["content"][block_index]["prompt_cache_breakpoint"] =
            serde_json::json!({ "mode": "explicit" });
    }
    Ok(())
}

/// Maximum explicit cache writes supported by GPT-5.6 Responses requests.
const OPENAI_EXPLICIT_PROMPT_CACHE_BREAKPOINT_LIMIT: usize = 4;

/// Selects the stationary stable boundary and up to three newest settled ones.
trait StableInputMarkerTarget {
    /// Returns ordered message and block coordinates for explicit breakpoints.
    fn stable_input_positions_marker_targets(
        &self,
        stable_input_positions: &[usize],
        settled_history_cache_checkpoint_positions: &[usize],
    ) -> Vec<(usize, usize)>;
}

impl StableInputMarkerTarget for [serde_json::Value] {
    fn stable_input_positions_marker_targets(
        &self,
        stable_input_positions: &[usize],
        settled_history_cache_checkpoint_positions: &[usize],
    ) -> Vec<(usize, usize)> {
        let stable_boundary = stable_input_positions
            .iter()
            .rev()
            .filter_map(|position| {
                let content = self.get(*position)?.get("content")?.as_array()?;
                let block_index = content.iter().rposition(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("input_text")
                })?;
                Some((*position, block_index))
            })
            .next();
        let mut targets = stable_boundary
            .into_iter()
            .chain(
                settled_history_cache_checkpoint_positions
                    .iter()
                    .rev()
                    .take(OPENAI_EXPLICIT_PROMPT_CACHE_BREAKPOINT_LIMIT - 1)
                    .filter_map(|position| {
                        let content = self.get(*position)?.get("content")?.as_array()?;
                        let block_index = content.iter().rposition(|block| {
                            block.get("type").and_then(serde_json::Value::as_str)
                                == Some("input_text")
                        })?;
                        Some((*position, block_index))
                    }),
            )
            .collect::<Vec<_>>();
        targets.sort_unstable();
        targets.dedup();
        targets
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
    openai_responses_request_body_with_stream_and_cache_comparison(request, stream, None)
}

/// Builds an OpenAI Responses request body with an optional diagnostic-only
/// baseline response identifier.
///
/// The comparison identifier asks the service to explain cache reuse; it never
/// changes model input, continuation state, or the prompt-cache routing key.
pub fn openai_responses_request_body_with_stream_and_cache_comparison(
    request: &ModelRequest,
    stream: bool,
    comparison_response_id: Option<&str>,
) -> ProviderRequestAssemblyResult<String> {
    validate_provider_request_required("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let mut body = openai_responses_request_control_shape_with_stream(request, stream)?;
    body["instructions"] = serde_json::json!(rendered.instructions);
    let mut input = rendered.input;
    apply_openai_prompt_cache_breakpoint(
        request,
        &rendered.stable_input_positions,
        &rendered.settled_history_cache_checkpoint_positions,
        &mut input,
    )?;
    body["input"] = serde_json::json!(input);
    body["prompt_cache_key"] = serde_json::json!(openai_prompt_cache_key(request));
    if body.get("prompt_cache_options").is_some()
        && let Some(comparison_response_id) = comparison_response_id.filter(|id| !id.is_empty())
    {
        body["prompt_cache_options"]["comparison_response_id"] =
            serde_json::json!(comparison_response_id);
    }
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

    /// Verifies a GPT-5.6 Responses comparison baseline remains a
    /// diagnostic-only cache option rather than prompt or continuation input.
    #[test]
    fn openai_responses_comparison_baseline_is_diagnostic_only_and_generation_gated() {
        let mut request = ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-5.6".to_string(),
            model_capabilities: Default::default(),
            max_input_tokens: None,
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: Some("lineage-1".to_string()),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: ModelInteractionKind::Compaction,
            allowed_actions: AllowedActionSet::all_enabled(),
            stop: None,
            messages: vec![crate::ModelMessage {
                role: crate::ModelMessageRole::User,
                source: crate::ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "compare cache reuse".to_string(),
            }]
            .into(),
        };

        let body: serde_json::Value = serde_json::from_str(
            &openai_responses_request_body_with_stream_and_cache_comparison(
                &request,
                false,
                Some("resp-baseline"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            body.pointer("/prompt_cache_options/comparison_response_id"),
            Some(&serde_json::json!("resp-baseline"))
        );
        assert!(!body["input"].to_string().contains("resp-baseline"));
        assert!(
            !body["prompt_cache_key"]
                .as_str()
                .unwrap()
                .contains("resp-baseline")
        );

        request.model = "gpt-5.5".to_string();
        let unsupported: serde_json::Value = serde_json::from_str(
            &openai_responses_request_body_with_stream_and_cache_comparison(
                &request,
                false,
                Some("resp-baseline"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            unsupported
                .pointer("/prompt_cache_options/comparison_response_id")
                .is_none()
        );
    }
}
