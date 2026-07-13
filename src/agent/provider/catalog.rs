//! Provider model catalog parsing and endpoint derivation.
//!
//! This module owns provider model catalog data structures, OpenAI-compatible
//! model endpoint derivation, model-list parsing, and reasoning-level metadata
//! extracted from catalog responses.

use super::{
    CHATGPT_RESPONSES_ENDPOINT, MezError, OPENAI_MODELS_ENDPOINT, OPENAI_RESPONSES_ENDPOINT,
    Result, validate_non_empty,
};
use crate::agent::known_model_context_window_tokens;

use mez_agent::{ProviderModelInfo, parse_openai_models_http_body_with};
pub use mez_agent::{openai_default_reasoning_levels_for_model, provider_catalog_reasoning_levels};

/// Derives the OpenAI Responses endpoint from a configured provider base URL.
///
/// Configuration names this value `base_url`, so a value such as
/// `https://api.openai.com/v1` is expanded to the documented
/// `https://api.openai.com/v1/responses` request endpoint. Existing endpoint
/// values ending in `/responses` are preserved.
pub fn openai_responses_endpoint_for_base_url(base_url: &str) -> Result<String> {
    validate_non_empty("OpenAI provider base URL", base_url)?;
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url == CHATGPT_RESPONSES_ENDPOINT
        || base_url.starts_with("https://chatgpt.com/backend-api/codex/")
    {
        return Err(MezError::invalid_state(
            "ChatGPT browser credentials do not expose an OpenAI-compatible base URL",
        ));
    }
    if base_url.ends_with("/responses") {
        return Ok(base_url.to_string());
    }
    if let Some(prefix) = base_url.strip_suffix("/models") {
        return Ok(format!("{prefix}/responses"));
    }
    Ok(format!("{base_url}/responses"))
}

/// Runs the openai models endpoint for responses endpoint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn openai_models_endpoint_for_responses_endpoint(endpoint: &str) -> Result<String> {
    validate_non_empty("OpenAI Responses endpoint", endpoint)?;
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint == CHATGPT_RESPONSES_ENDPOINT
        || endpoint.starts_with("https://chatgpt.com/backend-api/codex/")
    {
        return Err(MezError::invalid_state(
            "ChatGPT browser credentials do not expose an OpenAI-compatible model catalog",
        ));
    }
    if endpoint == OPENAI_RESPONSES_ENDPOINT {
        return Ok(OPENAI_MODELS_ENDPOINT.to_string());
    }
    if let Some(prefix) = endpoint.strip_suffix("/responses") {
        return Ok(format!("{prefix}/models"));
    }
    if endpoint.ends_with("/models") {
        return Ok(endpoint.to_string());
    }
    Ok(format!("{endpoint}/models"))
}

/// Runs the parse openai models http body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_openai_models_http_body(body: &str) -> Result<Vec<ProviderModelInfo>> {
    parse_openai_models_http_body_with(body, known_model_context_window_tokens).map_err(Into::into)
}
