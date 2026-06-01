//! Provider model catalog parsing and endpoint derivation.
//!
//! This module owns provider model catalog data structures, OpenAI-compatible
//! model endpoint derivation, model-list parsing, and reasoning-level metadata
//! extracted from catalog responses.

use super::{
    CHATGPT_RESPONSES_ENDPOINT, MezError, OPENAI_MODELS_ENDPOINT, OPENAI_RESPONSES_ENDPOINT,
    ProviderQuotaUsage, Result, validate_non_empty,
};
use crate::agent::known_model_context_window_tokens;

/// Carries Provider Model Info state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelInfo {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the display name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub display_name: Option<String>,
    /// Stores the reasoning levels value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reasoning_levels: Vec<String>,
    /// Provider-reported or locally documented context-window size in tokens.
    pub context_window_tokens: Option<usize>,
    /// Provider-reported capability tags for this model, such as LM Studio's
    /// `tool_use` marker.
    pub capabilities: Vec<String>,
}

/// Carries Provider Model Catalog state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCatalog {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: String,
    /// Stores the models value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub models: Vec<ProviderModelInfo>,
    /// Stores the reasoning levels value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reasoning_levels: Vec<String>,
    /// Provider-reported quota usage percentages for the catalog request.
    pub quota_usage: Vec<ProviderQuotaUsage>,
}

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
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!(
            "OpenAI Models response was not valid JSON: {error}"
        ))
    })?;
    let models = openai_models_array(&value)
        .ok_or_else(|| MezError::invalid_state("OpenAI Models response did not contain models"))?;
    let mut parsed = Vec::new();
    for model in models {
        if let Some(info) = openai_model_info_from_value(model) {
            parsed.push(info);
        }
    }
    parsed.sort_by(|left, right| left.id.cmp(&right.id));
    parsed.dedup_by(|left, right| left.id == right.id);
    Ok(parsed)
}

/// Runs the openai models array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_models_array(value: &serde_json::Value) -> Option<&[serde_json::Value]> {
    value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
}

/// Runs the openai model info from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_model_info_from_value(value: &serde_json::Value) -> Option<ProviderModelInfo> {
    let (id, display_name) = match value {
        serde_json::Value::String(model_id) => (model_id.to_string(), None),
        serde_json::Value::Object(object) => {
            let id = object
                .get("id")
                .or_else(|| object.get("name"))
                .or_else(|| object.get("slug"))
                .and_then(serde_json::Value::as_str)?
                .to_string();
            let display_name = object
                .get("display_name")
                .or_else(|| object.get("label"))
                .and_then(serde_json::Value::as_str)
                .filter(|name| *name != id)
                .map(str::to_string);
            (id, display_name)
        }
        _ => return None,
    };
    let mut reasoning_levels = provider_reasoning_levels_from_value(value);
    if reasoning_levels.is_empty() {
        reasoning_levels = openai_default_reasoning_levels_for_model(&id);
    }
    Some(ProviderModelInfo {
        id: id.clone(),
        display_name,
        reasoning_levels,
        context_window_tokens: provider_context_window_tokens_from_value(value)
            .or_else(|| known_model_context_window_tokens(&id)),
        capabilities: provider_capabilities_from_value(value),
    })
}

/// Returns provider-advertised model capability strings when present.
fn provider_capabilities_from_value(value: &serde_json::Value) -> Vec<String> {
    let mut capabilities = Vec::new();
    if let Some(values) = value
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
    {
        capabilities.extend(
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|capability| !capability.is_empty())
                .map(str::to_string),
        );
    }
    if let Some(object) = value
        .get("capabilities")
        .and_then(serde_json::Value::as_object)
    {
        capabilities.extend(
            object
                .iter()
                .filter(|(_, value)| value.as_bool().unwrap_or(false))
                .map(|(capability, _)| capability.trim())
                .filter(|capability| !capability.is_empty())
                .map(str::to_string),
        );
    }
    for field in ["tool_use", "tools", "function_calling", "structured_output"] {
        if value
            .get(field)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            capabilities.push(field.to_string());
        }
    }
    dedupe_provider_strings(capabilities)
}

/// Returns provider-advertised model context-window metadata when present.
fn provider_context_window_tokens_from_value(value: &serde_json::Value) -> Option<usize> {
    let object = value.as_object()?;
    for field in [
        "context_window_tokens",
        "context_limit_tokens",
        "context_window",
        "context_length",
        "max_context_length",
        "input_token_limit",
        "max_input_tokens",
    ] {
        if let Some(tokens) = object
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .and_then(|tokens| usize::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0)
        {
            return Some(tokens);
        }
    }
    for pointer in [
        "/limits/context_window_tokens",
        "/limits/context_limit_tokens",
        "/limits/context_window",
        "/limits/context_length",
        "/limits/max_context_length",
        "/capabilities/context_window_tokens",
        "/capabilities/context_limit_tokens",
        "/capabilities/context_window",
        "/capabilities/context_length",
        "/capabilities/max_context_length",
    ] {
        if let Some(tokens) = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_u64)
            .and_then(|tokens| usize::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0)
        {
            return Some(tokens);
        }
    }
    None
}

/// Runs the provider reasoning levels from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_reasoning_levels_from_value(value: &serde_json::Value) -> Vec<String> {
    for pointer in [
        "/reasoning/efforts",
        "/reasoning/levels",
        "/reasoning_efforts",
        "/reasoning_levels",
        "/supported_reasoning_efforts",
        "/supported_reasoning_levels",
        "/capabilities/reasoning_efforts",
        "/capabilities/reasoning_levels",
    ] {
        if let Some(levels) = value.pointer(pointer).and_then(serde_json::Value::as_array) {
            let levels = levels
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|level| !level.trim().is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if !levels.is_empty() {
                return dedupe_provider_strings(levels);
            }
        }
    }
    Vec::new()
}

/// Runs the openai default reasoning levels for model operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn openai_default_reasoning_levels_for_model(model_id: &str) -> Vec<String> {
    let lower = model_id.to_ascii_lowercase();
    if lower.starts_with("gpt-5")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
    {
        vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "xhigh".to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// Runs the provider catalog reasoning levels operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn provider_catalog_reasoning_levels(models: &[ProviderModelInfo]) -> Vec<String> {
    dedupe_provider_strings(
        models
            .iter()
            .flat_map(|model| model.reasoning_levels.iter().cloned())
            .collect(),
    )
}

/// Runs the dedupe provider strings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn dedupe_provider_strings(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !deduped.iter().any(|existing| existing == &value) {
            deduped.push(value);
        }
    }
    deduped
}
