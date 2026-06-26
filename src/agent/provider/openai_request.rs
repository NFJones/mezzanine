//! OpenAI Responses request construction.
//!
//! This module owns provider request-body construction for OpenAI Responses.
//! It depends on sibling modules for message rendering, cache keys, response
//! formatting, and MAAP schema selection so the provider facade can stay
//! focused on transport orchestration.

use super::cache::{
    openai_prompt_cache_key, openai_prompt_cache_retention_request_value,
    openai_render_request_messages, openai_response_format,
};
use super::schema::openai_maap_action_batch_tools;
use super::{OPENAI_MAAP_FUNCTION_TOOL_NAME, validate_non_empty};
use crate::agent::{ModelInteractionKind, ModelRequest};
use crate::error::{MezError, Result};

/// Builds a non-streaming OpenAI Responses request body.
///
/// The returned JSON includes the rendered prompt, prompt-cache routing key,
/// selected MAAP tool surface, response format, and provider-specific request
/// options derived from the model profile.
pub fn openai_responses_request_body(request: &ModelRequest) -> Result<String> {
    openai_responses_request_body_with_stream(request, false)
}

/// Builds an OpenAI Responses request body with explicit stream selection.
///
/// The provider facade uses this helper for HTTP request construction so the
/// streaming and non-streaming request shapes remain identical except for the
/// `stream` field.
pub(super) fn openai_responses_request_body_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> Result<String> {
    validate_non_empty("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let mut body = serde_json::json!({
        "model": request.model,
        "instructions": rendered.instructions,
        "input": rendered.input,
        "prompt_cache_key": openai_prompt_cache_key(request),
        "parallel_tool_calls": false,
        "store": false,
        "stream": stream
    });
    if let Some(response_format) = openai_response_format(request) {
        body["text"] = serde_json::json!({
            "format": response_format
        });
    }
    if let Some(effort) = request
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        body["reasoning"] = serde_json::json!({ "effort": effort });
    }
    if let Some(service_tier) =
        openai_service_tier_for_latency_preference(request.latency_preference.as_deref())?
    {
        body["service_tier"] = serde_json::json!(service_tier);
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        body["max_output_tokens"] = serde_json::json!(max_output_tokens);
    }
    if let Some(retention) = openai_prompt_cache_retention_request_value(request)? {
        body["prompt_cache_retention"] = serde_json::json!(retention);
    }
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        body["tool_choice"] = serde_json::json!("none");
    } else {
        body["tools"] = serde_json::json!(openai_maap_action_batch_tools(request));
        body["tool_choice"] = serde_json::json!({
            "type": "function",
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME
        });
    }
    serde_json::to_string(&body).map_err(|error| {
        MezError::invalid_state(format!("OpenAI request encoding failed: {error}"))
    })
}

/// Maps Mezzanine latency preferences to OpenAI Responses service tiers.
fn openai_service_tier_for_latency_preference(
    preference: Option<&str>,
) -> Result<Option<&'static str>> {
    match preference.map(str::trim).filter(|value| !value.is_empty()) {
        Some("slow") | Some("default") => Ok(Some("default")),
        None => Ok(None),
        Some("fast") => Ok(Some("priority")),
        Some(other) => Err(MezError::invalid_args(format!(
            "OpenAI latency_preference must be slow, default, or fast, got {other:?}"
        ))),
    }
}
