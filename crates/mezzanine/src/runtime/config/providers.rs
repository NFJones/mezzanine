//! Runtime provider and model-profile option readers.
//!
//! This module owns provider registry, model profile, and model preset
//! materialization from effective runtime configuration. Keeping provider
//! parsing here separates model-selection policy from terminal, frame, MCP,
//! permission, hook, and project-trust config domains.

use std::collections::BTreeMap;

use mez_agent::resolve_provider_api;
use mez_agent::{
    ModelPreset as RuntimeModelPreset, ModelProfile, ModelProfileDefinition,
    PresetRegistry as RuntimePresetRegistry, ProviderConfig as RuntimeProviderConfig,
    ProviderModelConfig as RuntimeProviderModelConfig, ProviderRegistry as RuntimeProviderRegistry,
};
use serde_json::Value;

use crate::error::{MezError, Result};

use super::{
    runtime_json_bool, runtime_json_object, runtime_json_string, runtime_json_string_array,
    runtime_json_string_map, runtime_validate_latency_preference,
};

pub(crate) fn runtime_provider_registry_from_config(
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
                    .map(|model| RuntimeProviderModelConfig::named(*model))
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
    let default_model = default_config.default_model.clone().unwrap_or_else(|| {
        default_config
            .models
            .first()
            .map(|model| model.id.clone())
            .unwrap_or_default()
    });
    let default_model = if default_model.is_empty() {
        runtime_recommended_model_for_provider(&default_config.kind)?.to_string()
    } else {
        default_model
    };
    registry.insert_profile_definition(
        default_profile.clone(),
        ModelProfileDefinition {
            provider: default_provider.to_string(),
            model: default_model,
            reasoning_profile: default_config.options.get("reasoning_effort").cloned(),
            latency_preference: Some("default".to_string()),
            ..ModelProfileDefinition::default()
        },
        None,
    )?;

    let synthesized_definitions = registry
        .providers
        .iter()
        .flat_map(|(provider_id, config)| {
            config
                .models
                .iter()
                .filter(|model| !model.id.is_empty())
                .map(|model| {
                    (
                        model.id.clone(),
                        ModelProfileDefinition {
                            provider: provider_id.clone(),
                            model: model.id.clone(),
                            reasoning_profile: config.options.get("reasoning_effort").cloned(),
                            latency_preference: Some("default".to_string()),
                            ..ModelProfileDefinition::default()
                        },
                    )
                })
        })
        .collect::<Vec<_>>();
    for (name, definition) in synthesized_definitions {
        if !registry.profile_definitions.contains_key(&name) {
            registry.insert_profile_definition(name, definition, None)?;
        }
    }

    if let Some(configured_profiles) = runtime_json_object(root, "model_profiles") {
        for (profile_name, value) in configured_profiles {
            let (definition, fallbacks) =
                runtime_model_profile_from_config(profile_name, value, &registry.providers)?;
            registry.insert_profile_definition(profile_name.clone(), definition, None)?;
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
pub(crate) fn runtime_preset_registry_from_config(
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
) -> Result<(ModelProfileDefinition, Vec<String>)> {
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
    let context_window_tokens = runtime_model_profile_context_window_tokens(profile_name, object)?;
    let max_input_tokens =
        runtime_model_profile_positive_token_count(profile_name, object, "max_input_tokens")?;
    let max_output_tokens =
        runtime_model_profile_positive_token_count(profile_name, object, "max_output_tokens")?;
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
        ModelProfileDefinition {
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
                .or_else(|| runtime_json_bool(object.get("multimodal"))),
            context_window_tokens,
            max_input_tokens,
            max_output_tokens,
            provider_options,
            safety_tier,
            ..ModelProfileDefinition::default()
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
    let models = runtime_provider_models_from_config(provider_id, object.get("models"))?;
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
    let config = RuntimeProviderConfig {
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
    };
    config
        .validate_models()
        .map_err(|error| MezError::config(error.to_string()))?;
    Ok(config)
}

/// Parses structured provider-model records while retaining compatibility with
/// already-normalized legacy arrays used by older in-memory test fixtures.
fn runtime_provider_models_from_config(
    provider_id: &str,
    value: Option<&Value>,
) -> Result<Vec<RuntimeProviderModelConfig>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_array() {
        return runtime_json_string_array(Some(value)).map(|models| {
            models
                .unwrap_or_default()
                .into_iter()
                .map(RuntimeProviderModelConfig::named)
                .collect()
        });
    }
    let models = value.as_object().ok_or_else(|| {
        MezError::config(format!(
            "providers.{provider_id}.models must be an object of model records"
        ))
    })?;
    let mut parsed = Vec::with_capacity(models.len());
    for (entry_id, value) in models {
        parsed.push(runtime_provider_model_from_config(
            provider_id,
            entry_id,
            value,
        )?);
    }
    Ok(parsed)
}

/// Parses one reusable provider-scoped model metadata record.
fn runtime_provider_model_from_config(
    provider_id: &str,
    entry_id: &str,
    value: &Value,
) -> Result<RuntimeProviderModelConfig> {
    let path = format!("providers.{provider_id}.models.{entry_id}");
    let model = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be an object")))?;
    let id = runtime_json_string(model.get("id"))
        .ok_or_else(|| MezError::config(format!("{path}.id is required")))?;
    let display_name = model
        .get("display_name")
        .map(|value| {
            runtime_json_string(Some(value))
                .map(ToOwned::to_owned)
                .ok_or_else(|| MezError::config(format!("{path}.display_name must be a string")))
        })
        .transpose()?;
    let aliases = runtime_json_string_array(model.get("aliases"))?.unwrap_or_default();
    let reasoning_levels = model
        .get("reasoning_levels")
        .map(|value| runtime_json_string_array(Some(value)).map(Option::unwrap_or_default))
        .transpose()?;
    let capabilities = model
        .get("capabilities")
        .map(|value| runtime_json_string_array(Some(value)).map(Option::unwrap_or_default))
        .transpose()?;
    let provider_options =
        runtime_json_string_map(model.get("provider_options"))?.unwrap_or_default();

    Ok(RuntimeProviderModelConfig {
        id: id.to_string(),
        display_name,
        aliases,
        context_window_tokens: runtime_provider_model_token_limit(
            model.get("context_window_tokens"),
            &format!("{path}.context_window_tokens"),
        )?,
        max_input_tokens: runtime_provider_model_token_limit(
            model.get("max_input_tokens"),
            &format!("{path}.max_input_tokens"),
        )?,
        max_output_tokens: runtime_provider_model_token_limit(
            model.get("max_output_tokens"),
            &format!("{path}.max_output_tokens"),
        )?,
        reasoning_levels,
        capabilities,
        provider_options,
    })
}

/// Parses one optional positive provider-model token limit.
fn runtime_provider_model_token_limit(value: Option<&Value>, path: &str) -> Result<Option<usize>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let tokens = value
        .as_u64()
        .and_then(|tokens| usize::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)
        .ok_or_else(|| MezError::config(format!("{path} must be a positive integer")))?;
    Ok(Some(tokens))
}

/// Returns the built-in model catalog for a provider kind.
///
/// The returned slice is used when a provider's configured `models` list is
/// empty, keeping local model selection useful without requiring a live
/// provider catalog request.
pub(crate) fn runtime_default_models_for_provider(kind: &str) -> Result<&'static [&'static str]> {
    match kind {
        "openai" => Ok(&[
            "gpt-5.6-terra",
            "gpt-5.6-sol",
            "gpt-5.6-luna",
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
        ]),
        "anthropic" => Ok(&[
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-fable-5",
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
pub(crate) fn runtime_recommended_model_for_provider(kind: &str) -> Result<&'static str> {
    runtime_default_models_for_provider(kind)?
        .first()
        .copied()
        .ok_or_else(|| MezError::config(format!("providers.{kind}.default_model is required")))
}

#[cfg(test)]
mod tests {
    use super::{runtime_provider_config_from_config, runtime_provider_registry_from_config};

    /// Verifies the generic streaming declaration survives provider config
    /// extraction as the string-valued compatibility option consumed by the
    /// OpenAI-compatible adapter.
    #[test]
    fn runtime_provider_config_preserves_generic_streaming_option() {
        let config = runtime_provider_config_from_config(
            "lmstudio",
            &serde_json::json!({
                "kind": "openai-compatible",
                "api": "openai-chat-completions",
                "options": { "streaming": "enabled" }
            }),
        )
        .unwrap();

        assert_eq!(
            config.options.get("streaming").map(String::as_str),
            Some("enabled")
        );
    }

    /// Verifies provider options remain string-only, so a bare TOML boolean
    /// cannot silently change the established dynamic option-map contract.
    #[test]
    fn runtime_provider_config_rejects_boolean_streaming_option() {
        let error = runtime_provider_config_from_config(
            "lmstudio",
            &serde_json::json!({
                "kind": "openai-compatible",
                "api": "openai-chat-completions",
                "options": { "streaming": true }
            }),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("providers.lmstudio.options.streaming must be a string")
        );
    }

    /// Verifies structured provider-model records materialize every reusable
    /// metadata field into the lower-crate configuration contract.
    #[test]
    fn runtime_provider_config_parses_structured_model_metadata() {
        let config = runtime_provider_config_from_config(
            "custom",
            &serde_json::json!({
                "kind": "openai-compatible",
                "models": {
                    "primary": {
                        "id": "model.a",
                        "display_name": "Model A",
                        "aliases": ["fast"],
                        "context_window_tokens": 200000,
                        "max_input_tokens": 180000,
                        "max_output_tokens": 16000,
                        "reasoning_levels": ["low", "high"],
                        "capabilities": ["vision", "tool_use"],
                        "provider_options": { "service_tier": "priority" }
                    }
                }
            }),
        )
        .unwrap();

        let model = &config.models[0];
        assert_eq!(model.id, "model.a");
        assert_eq!(model.display_name.as_deref(), Some("Model A"));
        assert_eq!(model.aliases, ["fast"]);
        assert_eq!(model.context_window_tokens, Some(200_000));
        assert_eq!(model.max_input_tokens, Some(180_000));
        assert_eq!(model.max_output_tokens, Some(16_000));
        assert_eq!(
            model.reasoning_levels,
            Some(vec!["low".to_string(), "high".to_string()])
        );
        assert_eq!(
            model.capabilities,
            Some(vec!["vision".to_string(), "tool_use".to_string()])
        );
        assert_eq!(model.provider_options["service_tier"], "priority");
    }

    /// Verifies direct runtime parsing rejects ambiguous model identities even
    /// when a caller bypasses the higher-level configuration validator.
    #[test]
    fn runtime_provider_config_rejects_model_identity_collisions() {
        let error = runtime_provider_config_from_config(
            "custom",
            &serde_json::json!({
                "kind": "openai-compatible",
                "models": {
                    "first": { "id": "model-a", "aliases": ["shared"] },
                    "second": { "id": "shared" }
                }
            }),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("provider model alias `shared` collides"),
            "{error}"
        );
    }

    /// Verifies configured named profiles inherit provider-model metadata and
    /// resolve aliases to the canonical provider-facing model id.
    #[test]
    fn runtime_registry_materializes_named_profiles_from_provider_models() {
        let registry = runtime_provider_registry_from_config(&serde_json::json!({
            "agents": {
                "default_provider": "custom",
                "default_model_profile": "work"
            },
            "providers": {
                "custom": {
                    "kind": "openai-compatible",
                    "default_model": "model-a",
                    "options": {
                        "root-only": "root",
                        "shared": "root"
                    },
                    "models": {
                        "primary": {
                            "id": "model-a",
                            "aliases": ["fast"],
                            "context_window_tokens": 200000,
                            "max_input_tokens": 180000,
                            "max_output_tokens": 8000,
                            "provider_options": {
                                "base-only": "base",
                                "shared": "base"
                            }
                        }
                    }
                }
            },
            "model_profiles": {
                "work": {
                    "provider": "custom",
                    "model": "fast",
                    "max_output_tokens": 16000,
                    "provider_options": {
                        "shared": "profile"
                    }
                }
            }
        }))
        .unwrap();

        let profile = registry.resolve_profile("work").unwrap();
        assert_eq!(profile.model, "model-a");
        assert_eq!(profile.context_window_tokens(), 200_000);
        assert_eq!(profile.max_input_tokens(), Some(180_000));
        assert_eq!(profile.max_output_tokens(), Some(16_000));
        assert_eq!(profile.provider_options["root-only"], "root");
        assert_eq!(profile.provider_options["base-only"], "base");
        assert_eq!(profile.provider_options["shared"], "profile");
    }
}
