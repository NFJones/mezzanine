//! OpenAI Responses request construction.
//!
//! This module owns provider request-body construction for OpenAI Responses.
//! It depends on sibling modules for message rendering, cache keys, response
//! formatting, and MAAP schema selection so the provider facade can stay
//! focused on transport orchestration.

use super::cache::{
    openai_prompt_cache_key, openai_prompt_cache_retention_request_value,
    openai_render_request_messages, openai_response_format,
    openai_service_tier_for_latency_preference,
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
    let mut body = openai_responses_request_control_shape_with_stream(request, stream)?;
    body["instructions"] = serde_json::json!(rendered.instructions);
    body["input"] = serde_json::json!(rendered.input);
    body["prompt_cache_key"] = serde_json::json!(openai_prompt_cache_key(request));
    serde_json::to_string(&body).map_err(|error| {
        MezError::invalid_state(format!("OpenAI request encoding failed: {error}"))
    })
}

/// Builds the canonical OpenAI request-control shape shared by request
/// emission and prompt-cache diagnostics.
pub(super) fn openai_responses_request_control_shape_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> Result<serde_json::Value> {
    validate_non_empty("OpenAI model", &request.model)?;
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
    Ok(body)
}
