//! Provider model catalog parsing and endpoint derivation.
//!
//! This module owns provider model catalog data structures, OpenAI-compatible
//! model endpoint derivation, model-list parsing, and reasoning-level metadata
//! extracted from catalog responses.

use super::Result;
use crate::agent::known_model_context_window_tokens;

use mez_agent::{
    ProviderModelInfo,
    openai_models_endpoint_for_responses_endpoint as derive_openai_models_endpoint,
    openai_responses_endpoint_for_base_url as derive_openai_responses_endpoint,
    parse_openai_models_http_body_with,
};
pub use mez_agent::{openai_default_reasoning_levels_for_model, provider_catalog_reasoning_levels};

/// Derives the OpenAI Responses endpoint from a configured provider base URL.
///
/// Configuration names this value `base_url`, so a value such as
/// `https://api.openai.com/v1` is expanded to the documented
/// `https://api.openai.com/v1/responses` request endpoint. Existing endpoint
/// values ending in `/responses` are preserved.
pub fn openai_responses_endpoint_for_base_url(base_url: &str) -> Result<String> {
    derive_openai_responses_endpoint(base_url).map_err(Into::into)
}

/// Runs the openai models endpoint for responses endpoint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn openai_models_endpoint_for_responses_endpoint(endpoint: &str) -> Result<String> {
    derive_openai_models_endpoint(endpoint).map_err(Into::into)
}

/// Runs the parse openai models http body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_openai_models_http_body(body: &str) -> Result<Vec<ProviderModelInfo>> {
    parse_openai_models_http_body_with(body, known_model_context_window_tokens).map_err(Into::into)
}
