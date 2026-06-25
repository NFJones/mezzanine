//! OpenAI Responses request construction.
//!
//! This module owns provider request-body construction for OpenAI Responses.
//! It depends on sibling modules for message rendering, cache keys, response
//! formatting, and MAAP schema selection so the provider facade can stay
//! focused on transport orchestration.

use super::cache::{
    openai_prompt_cache_key, openai_render_request_messages, openai_response_format,
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
    if let Some(retention) = request
        .prompt_cache_retention
        .as_deref()
        .filter(|retention| !retention.is_empty())
    {
        match retention {
            "in_memory" => {
                if openai_model_defaults_to_extended_prompt_cache_retention(&request.model) {
                    return Err(MezError::invalid_args(format!(
                        "OpenAI prompt_cache_retention \"in_memory\" is not supported for model {}; omit the option or use 24h",
                        request.model
                    )));
                }
            }
            "24h" => {
                if openai_model_supports_extended_prompt_cache_retention(&request.model) {
                    body["prompt_cache_retention"] = serde_json::json!(retention);
                } else {
                    return Err(MezError::invalid_args(format!(
                        "OpenAI prompt_cache_retention \"24h\" is not supported for model {}; omit the option or use in_memory",
                        request.model
                    )));
                }
            }
            other => {
                return Err(MezError::invalid_args(format!(
                    "OpenAI prompt_cache_retention must be in_memory or 24h, got {other:?}"
                )));
            }
        }
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

/// Reports whether one OpenAI model id is known to support extended prompt
/// cache retention.
///
/// Most supported model families still default to `in_memory`, but current
/// `gpt-5.5` and future GPT-family models default to extended retention and do
/// not accept an explicit `in_memory` request. The explicit `24h` request field
/// is accepted only for documented model families to avoid provider-side
/// unsupported-parameter failures on models that still support automatic prompt
/// caching without extended retention.
fn openai_model_supports_extended_prompt_cache_retention(model: &str) -> bool {
    let model = model.trim();
    openai_model_defaults_to_extended_prompt_cache_retention(model)
        || openai_model_matches_snapshot_family(model, "gpt-4.1")
        || openai_model_matches_snapshot_family(model, "gpt-5")
        || openai_model_matches_snapshot_family(model, "gpt-5-codex")
        || openai_model_matches_snapshot_family(model, "gpt-5.1")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-codex")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-codex-max")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-codex-mini")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-chat-latest")
        || openai_model_matches_snapshot_family(model, "gpt-5.2")
        || openai_model_matches_snapshot_family(model, "gpt-5.4")
}

/// Returns true when OpenAI documents extended retention as the default policy.
fn openai_model_defaults_to_extended_prompt_cache_retention(model: &str) -> bool {
    openai_gpt_model_version_at_least(model.trim(), 5, 5)
}

/// Matches an OpenAI model family exactly or by dated snapshot suffix.
///
/// This deliberately does not treat arbitrary named variants as members of a
/// documented family. For example, `gpt-5.4-2026-01-01` matches `gpt-5.4`, but
/// `gpt-5.4-mini` must be listed separately before Mezzanine sends `24h`.
fn openai_model_matches_snapshot_family(model: &str, family: &str) -> bool {
    model == family
        || model
            .strip_prefix(family)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(|first| first.is_ascii_digit())
}

/// Parses GPT-family versions and compares them with a minimum version.
fn openai_gpt_model_version_at_least(model: &str, min_major: u16, min_minor: u16) -> bool {
    let Some(rest) = model.strip_prefix("gpt-") else {
        return false;
    };
    let version = rest.split('-').next().unwrap_or_default();
    let mut parts = version.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u16>().ok()) else {
        return false;
    };
    let minor = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    major > min_major || (major == min_major && minor >= min_minor)
}
