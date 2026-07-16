//! Runtime provider and model-profile option readers.
//!
//! This module owns provider registry, model profile, and model preset
//! materialization from effective runtime configuration. Keeping provider
//! parsing here separates model-selection policy from terminal, frame, MCP,
//! permission, hook, and project-trust config domains.

use super::*;
use mez_agent::resolve_provider_api;

pub(in crate::runtime) fn runtime_provider_registry_from_config(
    root: &Value,
) -> Result<RuntimeProviderRegistry> {
    let agents = runtime_json_object(root, "agents");
    let default_provider = agents
        .and_then(|agents| runtime_json_string(agents.get("default_provider")))
        .unwrap_or("openai");
    let default_profile = agents
        .and_then(|agents| runtime_json_string(agents.get("default_model_profile")))
        .unwrap_or("default")
        .to_string();
    let mut registry = RuntimeProviderRegistry {
        default_profile: Some(default_profile.clone()),
        ..RuntimeProviderRegistry::default()
    };

    if let Some(providers) = runtime_json_object(root, "providers") {
        for (provider_id, value) in providers {
            let config = runtime_provider_config_from_config(provider_id, value)?;
            registry.providers.insert(provider_id.clone(), config);
        }
    }

    if registry.providers.is_empty() {
        registry.providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                provider_id: "openai".to_string(),
                kind: "openai".to_string(),
                api: None,
                auth_profile: "default".to_string(),
                base_url: None,
                models: runtime_default_models_for_provider("openai")?
                    .iter()
                    .map(|model| (*model).to_string())
                    .collect(),
                default_model: Some(runtime_recommended_model_for_provider("openai")?.to_string()),
                options: BTreeMap::new(),
            },
        );
    }

    let default_config = registry.providers.get(default_provider).ok_or_else(|| {
        MezError::config(format!(
            "agents.default_provider `{default_provider}` is not configured in providers"
        ))
    })?;
    let default_model = default_config
        .default_model
        .clone()
        .unwrap_or_else(|| default_config.models.first().cloned().unwrap_or_default());
    let default_model = if default_model.is_empty() {
        runtime_recommended_model_for_provider(&default_config.kind)?.to_string()
    } else {
        default_model
    };
    registry.profiles.insert(
        default_profile.clone(),
        ModelProfile {
            provider: default_provider.to_string(),
            model: default_model,
            reasoning_profile: default_config.options.get("reasoning_effort").cloned(),
            latency_preference: Some("default".to_string()),
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
    );

    for (provider_id, config) in &registry.providers {
        for model in &config.models {
            if model.is_empty() {
                continue;
            }
            registry
                .profiles
                .entry(model.clone())
                .or_insert(ModelProfile {
                    provider: provider_id.clone(),
                    model: model.clone(),
                    reasoning_profile: config.options.get("reasoning_effort").cloned(),
                    latency_preference: Some("default".to_string()),
                    multimodal_required: false,
                    provider_options: std::collections::BTreeMap::new(),
                    safety_tier: None,
                });
        }
    }

    if let Some(configured_profiles) = runtime_json_object(root, "model_profiles") {
        for (profile_name, value) in configured_profiles {
            let (profile, fallbacks) =
                runtime_model_profile_from_config(profile_name, value, &registry.providers)?;
            registry.profiles.insert(profile_name.clone(), profile);
            if !fallbacks.is_empty() {
                registry
                    .fallback_profiles
                    .insert(profile_name.clone(), fallbacks);
            }
        }
    }
    if !registry.profiles.contains_key(&default_profile) {
        return Err(MezError::config(format!(
            "agents.default_model_profile `{default_profile}` is not configured in model_profiles"
        )));
    }
    for (profile_name, fallbacks) in &registry.fallback_profiles {
        for fallback in fallbacks {
            if !registry.profiles.contains_key(fallback) {
                return Err(MezError::config(format!(
                    "model_profiles.{profile_name}.fallback_profiles references unknown model profile `{fallback}`"
                )));
            }
        }
    }

    Ok(registry)
}

/// Parses model presets from the config root.
pub(in crate::runtime) fn runtime_preset_registry_from_config(
    root: &Value,
    profiles: &BTreeMap<String, ModelProfile>,
) -> Result<RuntimePresetRegistry> {
    let mut registry = RuntimePresetRegistry::default();
    let Some(presets) = runtime_json_object(root, "model_presets") else {
        return Ok(registry);
    };
    for (preset_name, value) in presets {
        let object = value.as_object().ok_or_else(|| {
            MezError::config(format!("model_presets.{preset_name} must be a table"))
        })?;
        let default_model_profile = runtime_json_string(object.get("default_model_profile"))
            .ok_or_else(|| {
                MezError::config(format!(
                    "model_presets.{preset_name}.default_model_profile is required"
                ))
            })?;
        if !profiles.contains_key(default_model_profile) {
            return Err(MezError::config(format!(
                "model_presets.{preset_name}.default_model_profile `{default_model_profile}` is not configured in model_profiles"
            )));
        }
        let auto_sizing_router_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_router_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let auto_sizing_small_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_small_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let auto_sizing_medium_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_medium_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let auto_sizing_large_model_profile = runtime_preset_model_profile_reference(
            preset_name,
            "auto_sizing_large_model_profile",
            object,
            profiles,
            default_model_profile,
        )?;
        let allowed_reasoning_efforts =
            runtime_json_string_array(object.get("allowed_reasoning_efforts"))?.unwrap_or_default();
        for effort in &allowed_reasoning_efforts {
            if !matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh") {
                return Err(MezError::config(format!(
                    "model_presets.{preset_name}.allowed_reasoning_efforts contains unsupported effort `{effort}`"
                )));
            }
        }
        let preset = RuntimeModelPreset {
            default_model_profile: default_model_profile.to_string(),
            auto_sizing_router_model_profile,
            auto_sizing_small_model_profile,
            auto_sizing_medium_model_profile,
            auto_sizing_large_model_profile,
            allowed_reasoning_efforts,
        };
        registry.presets.insert(preset_name.clone(), preset);
    }
    Ok(registry)
}

/// Parses and validates one model-profile reference from a model preset.
fn runtime_preset_model_profile_reference(
    preset_name: &str,
    key: &str,
    object: &serde_json::Map<String, Value>,
    profiles: &BTreeMap<String, ModelProfile>,
    fallback: &str,
) -> Result<String> {
    let profile = runtime_json_string(object.get(key)).unwrap_or(fallback);
    if profile.trim().is_empty() {
        return Err(MezError::config(format!(
            "model_presets.{preset_name}.{key} must not be empty"
        )));
    }
    if !profiles.contains_key(profile) {
        return Err(MezError::config(format!(
            "model_presets.{preset_name}.{key} `{profile}` is not configured in model_profiles"
        )));
    }
    Ok(profile.to_string())
}

/// Runs the runtime model profile from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_model_profile_from_config(
    profile_name: &str,
    value: &Value,
    providers: &BTreeMap<String, RuntimeProviderConfig>,
) -> Result<(ModelProfile, Vec<String>)> {
    let Some(object) = value.as_object() else {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name} must be an object"
        )));
    };
    let provider = runtime_json_string(object.get("provider")).ok_or_else(|| {
        MezError::config(format!(
            "model_profiles.{profile_name}.provider is required"
        ))
    })?;
    if !providers.contains_key(provider) {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.provider `{provider}` is not configured"
        )));
    }
    let model = runtime_json_string(object.get("model")).ok_or_else(|| {
        MezError::config(format!("model_profiles.{profile_name}.model is required"))
    })?;
    let mut provider_options =
        runtime_json_string_map(object.get("provider_options"))?.unwrap_or_default();
    if let Some(privacy_tier) = runtime_json_string(object.get("privacy_tier")) {
        provider_options
            .entry("privacy_tier".to_string())
            .or_insert_with(|| privacy_tier.to_string());
    }
    if let Some(residency) = runtime_json_string(object.get("residency")) {
        provider_options
            .entry("residency".to_string())
            .or_insert_with(|| residency.to_string());
    }
    if let Some(approval_policy) = runtime_json_string(object.get("approval_policy")) {
        provider_options
            .entry("approval_policy".to_string())
            .or_insert_with(|| approval_policy.to_string());
    }
    if let Some(context_window_tokens) =
        runtime_model_profile_context_window_tokens(profile_name, object)?
    {
        provider_options
            .entry("context_window_tokens".to_string())
            .or_insert_with(|| context_window_tokens.to_string());
    }
    if let Some(max_output_tokens) =
        runtime_model_profile_positive_token_count(profile_name, object, "max_output_tokens")?
    {
        provider_options
            .entry("max_output_tokens".to_string())
            .or_insert_with(|| max_output_tokens.to_string());
    }
    let safety_tier = runtime_json_string(object.get("safety_tier")).map(str::to_string);
    if let Some(safety_tier) = safety_tier.as_deref()
        && !matches!(safety_tier, "basic" | "medium" | "high")
    {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.safety_tier must be basic, medium, or high"
        )));
    }
    let fallbacks = runtime_json_string_array(object.get("fallback_profiles"))?.unwrap_or_default();
    Ok((
        ModelProfile {
            provider: provider.to_string(),
            model: model.to_string(),
            reasoning_profile: runtime_json_string(object.get("reasoning_profile"))
                .or_else(|| runtime_json_string(object.get("reasoning_effort")))
                .or_else(|| provider_options.get("reasoning_effort").map(String::as_str))
                .map(str::to_string),
            latency_preference: Some(
                runtime_validate_latency_preference(
                    runtime_json_string(object.get("latency_preference")).unwrap_or("default"),
                )?
                .to_string(),
            ),
            multimodal_required: runtime_json_bool(object.get("multimodal_required"))
                .or_else(|| runtime_json_bool(object.get("multimodal")))
                .unwrap_or(false),
            provider_options,
            safety_tier,
        },
        fallbacks,
    ))
}

/// Parses model-profile context window configuration as a positive token count.
fn runtime_model_profile_context_window_tokens(
    profile_name: &str,
    object: &serde_json::Map<String, Value>,
) -> Result<Option<usize>> {
    runtime_model_profile_positive_token_count_with_aliases(
        profile_name,
        object,
        &["context_window_tokens", "context_limit_tokens"],
    )
}

/// Parses a positive model-profile token count from one key.
fn runtime_model_profile_positive_token_count(
    profile_name: &str,
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<usize>> {
    runtime_model_profile_positive_token_count_with_aliases(profile_name, object, &[key])
}

/// Parses a positive model-profile token count from one or more equivalent
/// keys.
fn runtime_model_profile_positive_token_count_with_aliases(
    profile_name: &str,
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Result<Option<usize>> {
    let Some((key, value)) = keys
        .iter()
        .find_map(|key| object.get(*key).map(|value| (*key, value)))
    else {
        return Ok(None);
    };
    let tokens = if let Some(tokens) = value.as_u64() {
        tokens
    } else if let Some(tokens) = runtime_json_string(Some(value)) {
        tokens.parse::<u64>().map_err(|_| {
            MezError::config(format!(
                "model_profiles.{profile_name}.{key} must be a positive integer"
            ))
        })?
    } else {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.{key} must be a positive integer"
        )));
    };
    let tokens = usize::try_from(tokens).map_err(|_| {
        MezError::config(format!("model_profiles.{profile_name}.{key} is too large"))
    })?;
    if tokens == 0 {
        return Err(MezError::config(format!(
            "model_profiles.{profile_name}.{key} must be greater than zero"
        )));
    }
    Ok(Some(tokens))
}

/// Runs the runtime provider config from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_provider_config_from_config(
    provider_id: &str,
    value: &Value,
) -> Result<RuntimeProviderConfig> {
    let Some(object) = value.as_object() else {
        return Err(MezError::config(format!(
            "providers.{provider_id} must be an object"
        )));
    };
    let kind = runtime_json_string(object.get("kind")).unwrap_or(provider_id);
    let api = runtime_json_string(object.get("api")).map(ToOwned::to_owned);
    resolve_provider_api(kind, api.as_deref())?;
    let models = runtime_json_string_array(object.get("models"))?.unwrap_or_default();
    let default_model = runtime_json_string(object.get("default_model"))
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned);
    let mut options = BTreeMap::new();
    if let Some(option_map) = object.get("options").and_then(Value::as_object) {
        for (key, value) in option_map {
            let Some(value) = runtime_json_string(Some(value)) else {
                return Err(MezError::config(format!(
                    "providers.{provider_id}.options.{key} must be a string"
                )));
            };
            options.insert(key.clone(), value.to_string());
        }
    }
    Ok(RuntimeProviderConfig {
        provider_id: provider_id.to_string(),
        kind: kind.to_string(),
        api,
        auth_profile: runtime_json_string(object.get("auth_profile"))
            .unwrap_or("default")
            .to_string(),
        base_url: runtime_json_string(object.get("base_url")).map(ToOwned::to_owned),
        models,
        default_model,
        options,
    })
}

/// Returns the built-in model catalog for a provider kind.
///
/// The returned slice is used when a provider's configured `models` list is
/// empty, keeping local model selection useful without requiring a live
/// provider catalog request.
pub(crate) fn runtime_default_models_for_provider(kind: &str) -> Result<&'static [&'static str]> {
    match kind {
        "openai" => Ok(&[
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
        ]),
        "anthropic" => Ok(&[
            "claude-fable-5",
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
        ]),
        "claude-code" => Ok(&[
            "claude-fable-5",
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
        ]),
        "deepseek" => Ok(&["deepseek-v4-pro", "deepseek-v4-flash"]),
        _ => Err(MezError::config(format!(
            "providers.{kind}.models is required for provider kind `{kind}`"
        ))),
    }
}

/// Runs the runtime recommended model for provider operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_recommended_model_for_provider(
    kind: &str,
) -> Result<&'static str> {
    runtime_default_models_for_provider(kind)?
        .first()
        .copied()
        .ok_or_else(|| MezError::config(format!("providers.{kind}.default_model is required")))
}
