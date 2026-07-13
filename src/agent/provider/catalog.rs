//! Product adapter for provider model catalog parsing.
//!
//! Provider-neutral endpoint derivation, model-list parsing, and reasoning
//! metadata belong to `mez-agent`. This adapter supplies Mezzanine's known
//! model context-window fallback and converts lower-crate errors at the
//! composition boundary.

use super::Result;
use mez_agent::{
    ProviderModelInfo, known_model_context_window_tokens, parse_openai_models_http_body_with,
};

/// Runs the parse openai models http body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_openai_models_http_body(body: &str) -> Result<Vec<ProviderModelInfo>> {
    parse_openai_models_http_body_with(body, known_model_context_window_tokens).map_err(Into::into)
}
