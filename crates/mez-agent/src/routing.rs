//! Provider profile registry and model-preset routing policy.
//!
//! This module owns provider-independent configured records, profile lookup,
//! failover safety filtering, and preset resolution. Product configuration
//! parsing, credentials, transport construction, and provider invocation stay
//! in the root package.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::{ModelCatalog, ModelProfile};

/// Reusable provider-scoped base metadata for one canonical model.
///
/// Optional fields preserve the distinction between omitted metadata and an
/// explicitly configured empty list. Product configuration adapters validate
/// richer format-specific constraints before constructing this record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderModelConfig {
    /// Canonical provider-facing model identifier.
    pub id: String,
    /// Optional user-facing display label.
    pub display_name: Option<String>,
    /// Alternate provider-local identifiers for model selection.
    pub aliases: Vec<String>,
    /// Optional positive context-window size in tokens.
    pub context_window_tokens: Option<usize>,
    /// Optional positive maximum request-input size in tokens.
    pub max_input_tokens: Option<usize>,
    /// Optional positive maximum response-output size in tokens.
    pub max_output_tokens: Option<usize>,
    /// Optional replacement list of supported reasoning levels.
    pub reasoning_levels: Option<Vec<String>>,
    /// Optional replacement list of provider-neutral capability tags.
    pub capabilities: Option<Vec<String>>,
    /// Secret-free model-level provider option defaults.
    pub provider_options: BTreeMap<String, String>,
}

impl ProviderModelConfig {
    /// Creates a minimal configured model with one canonical identifier.
    pub fn named(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }
}

/// Explicit model-profile configuration before provider-model inheritance.
///
/// Optional fields retain omission separately from explicit profile policy so
/// rematerialization can rebase the definition against updated model metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelProfileDefinition {
    /// Configured provider identity.
    pub provider: String,
    /// Configured canonical model id, alias, or unlisted custom model id.
    pub model: String,
    /// Optional selected reasoning effort.
    pub reasoning_profile: Option<String>,
    /// Optional latency preference.
    pub latency_preference: Option<String>,
    /// Optional multimodal requirement override.
    pub multimodal_required: Option<bool>,
    /// Optional context-window override.
    pub context_window_tokens: Option<usize>,
    /// Optional maximum request-input override.
    pub max_input_tokens: Option<usize>,
    /// Optional maximum response-output override.
    pub max_output_tokens: Option<usize>,
    /// Optional replacement list of supported reasoning levels.
    pub reasoning_levels: Option<Vec<String>>,
    /// Optional replacement list of provider-neutral capability tags.
    pub capabilities: Option<Vec<String>>,
    /// Secret-free profile-level provider options.
    pub provider_options: BTreeMap<String, String>,
    /// Optional safety tier used by failover policy.
    pub safety_tier: Option<String>,
}

/// Stable category for invalid provider-local model identity metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderModelConfigErrorKind {
    /// A canonical model identifier was empty.
    EmptyId,
    /// Two model records declared the same canonical identifier.
    DuplicateId,
    /// An alias was empty.
    EmptyAlias,
    /// An alias overlapped another canonical identifier or alias.
    AliasCollision,
}

/// Failure returned when provider-local model identities are ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelConfigError {
    kind: ProviderModelConfigErrorKind,
    message: String,
}

impl ProviderModelConfigError {
    /// Returns the stable validation failure category.
    pub fn kind(&self) -> ProviderModelConfigErrorKind {
        self.kind
    }

    /// Returns the diagnostic message for product error projection.
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: ProviderModelConfigErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ProviderModelConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ProviderModelConfigError {}

/// Failure returned by provider-profile routing policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRoutingError {
    message: String,
}

impl ProviderRoutingError {
    /// Returns the diagnostic message for product error projection.
    pub fn message(&self) -> &str {
        &self.message
    }

    fn profile_not_configured(profile_name: &str) -> Self {
        Self {
            message: format!("model profile `{profile_name}` is not configured"),
        }
    }

    fn materialization(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProviderRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ProviderRoutingError {}

/// Result returned by provider-profile routing policy.
pub type ProviderRoutingResult<T> = Result<T, ProviderRoutingError>;

/// Secret-free provider configuration used by routing and profile selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Stable provider identity.
    pub provider_id: String,
    /// Configured provider implementation kind.
    pub kind: String,
    /// Optional API compatibility selector.
    pub api: Option<String>,
    /// Product auth-profile identity; no credential value is stored here.
    pub auth_profile: String,
    /// Optional configured provider endpoint.
    pub base_url: Option<String>,
    /// Configured provider-scoped model metadata.
    pub models: Vec<ProviderModelConfig>,
    /// Optional default model name.
    pub default_model: Option<String>,
    /// Secret-free provider options used by request policy.
    pub options: BTreeMap<String, String>,
}

impl ProviderConfig {
    /// Validates canonical model identifiers and aliases within this provider.
    pub fn validate_models(&self) -> Result<(), ProviderModelConfigError> {
        let mut identities = BTreeMap::<String, &'static str>::new();
        for model in &self.models {
            let id = model.id.trim();
            if id.is_empty() {
                return Err(ProviderModelConfigError::new(
                    ProviderModelConfigErrorKind::EmptyId,
                    "provider model id must not be empty",
                ));
            }
            if identities.insert(id.to_string(), "id").is_some() {
                return Err(ProviderModelConfigError::new(
                    ProviderModelConfigErrorKind::DuplicateId,
                    format!("provider model id `{id}` is configured more than once"),
                ));
            }
        }
        for model in &self.models {
            for alias in &model.aliases {
                let alias = alias.trim();
                if alias.is_empty() {
                    return Err(ProviderModelConfigError::new(
                        ProviderModelConfigErrorKind::EmptyAlias,
                        format!("provider model `{}` has an empty alias", model.id.trim()),
                    ));
                }
                if identities.insert(alias.to_string(), "alias").is_some() {
                    return Err(ProviderModelConfigError::new(
                        ProviderModelConfigErrorKind::AliasCollision,
                        format!("provider model alias `{alias}` collides with another identity"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Resolves a canonical model identifier or alias to its model record.
    ///
    /// Identity metadata is validated before lookup so ambiguous records never
    /// produce order-dependent selection. An unknown or empty request returns
    /// `Ok(None)`.
    pub fn model(
        &self,
        requested: &str,
    ) -> Result<Option<&ProviderModelConfig>, ProviderModelConfigError> {
        self.validate_models()?;
        let requested = requested.trim();
        if requested.is_empty() {
            return Ok(None);
        }
        Ok(self.models.iter().find(|model| {
            model.id.trim() == requested
                || model.aliases.iter().any(|alias| alias.trim() == requested)
        }))
    }
}

/// Provider and model-profile registry used by routing policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderRegistry {
    /// Optional default profile identity.
    pub default_profile: Option<String>,
    /// Provider records keyed by provider identity.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Model profiles keyed by configured profile identity.
    pub profiles: BTreeMap<String, ModelProfile>,
    /// Override-only profile definitions keyed by configured profile identity.
    pub profile_definitions: BTreeMap<String, ModelProfileDefinition>,
    /// Last provider catalogs used to fill definition gaps.
    pub profile_catalogs: BTreeMap<String, ModelCatalog>,
    /// Ordered fallback profile identities keyed by preferred profile.
    pub fallback_profiles: BTreeMap<String, Vec<String>>,
}

impl ProviderRegistry {
    /// Returns the configured default profile identity, when present.
    pub fn default_profile_name(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    /// Returns a provider record by identity.
    pub fn provider(&self, provider_id: &str) -> Option<&ProviderConfig> {
        self.providers.get(provider_id)
    }

    /// Returns a model profile by configured identity.
    pub fn profile(&self, profile_name: &str) -> Option<&ModelProfile> {
        self.profiles.get(profile_name)
    }

    /// Resolves and clones a configured model profile.
    ///
    /// Returns an error when the profile identity is not configured.
    pub fn resolve_profile(&self, profile_name: &str) -> ProviderRoutingResult<ModelProfile> {
        self.profile(profile_name)
            .cloned()
            .ok_or_else(|| ProviderRoutingError::profile_not_configured(profile_name))
    }

    /// Returns all configured provider records.
    pub fn providers(&self) -> &BTreeMap<String, ProviderConfig> {
        &self.providers
    }

    /// Returns all configured model profiles.
    pub fn profiles(&self) -> &BTreeMap<String, ModelProfile> {
        &self.profiles
    }

    /// Returns override-only model-profile definitions.
    pub fn profile_definitions(&self) -> &BTreeMap<String, ModelProfileDefinition> {
        &self.profile_definitions
    }

    /// Inserts one definition and its newly materialized effective profile.
    pub fn insert_profile_definition(
        &mut self,
        profile_name: impl Into<String>,
        definition: ModelProfileDefinition,
        catalog: Option<&ModelCatalog>,
    ) -> ProviderRoutingResult<()> {
        let profile_name = profile_name.into();
        let profile = self.materialize_profile_definition(&definition, catalog)?;
        self.profile_definitions
            .insert(profile_name.clone(), definition);
        self.profiles.insert(profile_name, profile);
        Ok(())
    }

    /// Rematerializes future profile resolutions for one provider.
    ///
    /// Existing cloned profiles owned by in-flight turns are values outside the
    /// registry and therefore remain pinned to their original metadata.
    pub fn rematerialize_profiles_for_provider(
        &mut self,
        provider_id: &str,
        catalog: Option<&ModelCatalog>,
    ) -> ProviderRoutingResult<()> {
        if let Some(catalog) = catalog {
            self.profile_catalogs
                .insert(provider_id.to_string(), catalog.clone());
        } else {
            self.profile_catalogs.remove(provider_id);
        }
        let catalog = self.profile_catalogs.get(provider_id);
        let definitions = self
            .profile_definitions
            .iter()
            .filter(|(_name, definition)| definition.provider == provider_id)
            .map(|(name, definition)| (name.clone(), definition.clone()))
            .collect::<Vec<_>>();
        let materialized = definitions
            .into_iter()
            .map(|(name, definition)| {
                self.materialize_profile_definition(&definition, catalog)
                    .map(|profile| (name, profile))
            })
            .collect::<ProviderRoutingResult<Vec<_>>>()?;
        for (name, profile) in materialized {
            self.profiles.insert(name, profile);
        }
        Ok(())
    }

    /// Materializes one override-only definition against provider/model data.
    pub fn materialize_profile_definition(
        &self,
        definition: &ModelProfileDefinition,
        catalog: Option<&ModelCatalog>,
    ) -> ProviderRoutingResult<ModelProfile> {
        let provider = self.providers.get(&definition.provider).ok_or_else(|| {
            ProviderRoutingError::materialization(format!(
                "model profile provider `{}` is not configured",
                definition.provider
            ))
        })?;
        let configured_model = provider
            .model(&definition.model)
            .map_err(|error| ProviderRoutingError::materialization(error.to_string()))?;
        let canonical_model = configured_model
            .map(|model| model.id.as_str())
            .unwrap_or(definition.model.as_str());
        let catalog_model = catalog.and_then(|catalog| {
            catalog
                .resolve(canonical_model)
                .or_else(|| catalog.resolve(&definition.model))
        });
        let model = configured_model
            .map(|model| model.id.clone())
            .or_else(|| catalog_model.map(|model| model.id.clone()))
            .unwrap_or_else(|| definition.model.clone());

        let mut provider_options = provider.options.clone();
        if let Some(catalog_model) = catalog_model {
            provider_options.extend(catalog_model.provider_options.clone());
        }
        if let Some(configured_model) = configured_model {
            provider_options.extend(configured_model.provider_options.clone());
        }

        let context_window_tokens = definition
            .context_window_tokens
            .or_else(|| configured_model.and_then(|model| model.context_window_tokens))
            .or_else(|| catalog_model.and_then(|model| model.context_window_tokens));
        let max_input_tokens = definition
            .max_input_tokens
            .or_else(|| configured_model.and_then(|model| model.max_input_tokens))
            .or_else(|| catalog_model.and_then(|model| model.max_input_tokens));
        let max_output_tokens = definition
            .max_output_tokens
            .or_else(|| configured_model.and_then(|model| model.max_output_tokens))
            .or_else(|| catalog_model.and_then(|model| model.max_output_tokens));
        insert_profile_limit(
            &mut provider_options,
            "context_window_tokens",
            context_window_tokens,
        );
        insert_profile_limit(&mut provider_options, "max_input_tokens", max_input_tokens);
        insert_profile_limit(
            &mut provider_options,
            "max_output_tokens",
            max_output_tokens,
        );

        let reasoning_levels = definition
            .reasoning_levels
            .as_ref()
            .or_else(|| configured_model.and_then(|model| model.reasoning_levels.as_ref()))
            .map(Vec::as_slice)
            .or_else(|| catalog_model.map(|model| model.reasoning_levels.as_slice()));
        let capabilities = definition
            .capabilities
            .as_ref()
            .or_else(|| configured_model.and_then(|model| model.capabilities.as_ref()))
            .map(Vec::as_slice)
            .or_else(|| catalog_model.map(|model| model.capabilities.as_slice()));
        if let Some(reasoning_levels) = reasoning_levels {
            provider_options.insert(
                "model_reasoning_levels".to_string(),
                reasoning_levels.join(","),
            );
        }
        if let Some(capabilities) = capabilities {
            provider_options.insert("model_capabilities".to_string(), capabilities.join(","));
        }
        provider_options.extend(definition.provider_options.clone());

        Ok(ModelProfile {
            provider: definition.provider.clone(),
            model,
            reasoning_profile: definition.reasoning_profile.clone(),
            latency_preference: definition.latency_preference.clone(),
            multimodal_required: definition.multimodal_required.unwrap_or(false),
            provider_options,
            safety_tier: definition.safety_tier.clone(),
        })
    }

    /// Returns configured fallbacks that are not weaker than the preferred profile.
    ///
    /// Missing preferred or fallback profiles return a typed routing error.
    pub fn safe_fallback_profiles(&self, profile_name: &str) -> ProviderRoutingResult<Vec<String>> {
        let preferred = self.resolve_profile(profile_name)?;
        let Some(fallbacks) = self.fallback_profiles.get(profile_name) else {
            return Ok(Vec::new());
        };
        let mut safe = Vec::new();
        for fallback_name in fallbacks {
            let fallback = self.resolve_profile(fallback_name)?;
            if preferred.failover_safe(&fallback) {
                safe.push(fallback_name.clone());
            }
        }
        Ok(safe)
    }
}

/// Inserts one inherited positive model limit into effective profile options.
fn insert_profile_limit(
    provider_options: &mut BTreeMap<String, String>,
    key: &str,
    value: Option<usize>,
) {
    if let Some(value) = value {
        provider_options.insert(key.to_string(), value.to_string());
    }
}

/// Named model-preset configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPreset {
    /// Primary model profile to use.
    pub default_model_profile: String,
    /// Auto-sizing router model profile.
    pub auto_sizing_router_model_profile: String,
    /// Auto-sizing small model profile.
    pub auto_sizing_small_model_profile: String,
    /// Auto-sizing medium model profile.
    pub auto_sizing_medium_model_profile: String,
    /// Auto-sizing large model profile.
    pub auto_sizing_large_model_profile: String,
    /// Reasoning efforts allowed for auto-sizing.
    pub allowed_reasoning_efforts: Vec<String>,
}

/// Model-preset registry keyed by preset identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetRegistry {
    /// Named model presets keyed by preset identity.
    pub presets: BTreeMap<String, ModelPreset>,
}

impl PresetRegistry {
    /// Returns true when at least one preset is defined.
    pub fn has_presets(&self) -> bool {
        !self.presets.is_empty()
    }

    /// Resolves a preset by name.
    pub fn resolve(&self, name: &str) -> Option<&ModelPreset> {
        self.presets.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies fallback routing removes profiles with a weaker safety tier.
    #[test]
    fn provider_registry_filters_unsafe_fallback_profiles() {
        let profile = |tier: &str| ModelProfile {
            safety_tier: Some(tier.to_string()),
            ..ModelProfile::default()
        };
        let registry = ProviderRegistry {
            profiles: BTreeMap::from([
                ("preferred".to_string(), profile("high")),
                ("safe".to_string(), profile("high")),
                ("weak".to_string(), profile("basic")),
            ]),
            fallback_profiles: BTreeMap::from([(
                "preferred".to_string(),
                vec!["safe".to_string(), "weak".to_string()],
            )]),
            ..ProviderRegistry::default()
        };

        assert_eq!(
            registry.safe_fallback_profiles("preferred").unwrap(),
            vec!["safe".to_string()]
        );
    }

    /// Verifies missing profiles return a stable typed routing diagnostic.
    #[test]
    fn provider_registry_rejects_missing_profiles() {
        let error = ProviderRegistry::default()
            .resolve_profile("missing")
            .unwrap_err();
        assert_eq!(error.message(), "model profile `missing` is not configured");
    }

    /// Verifies provider-model lookup accepts canonical identifiers and aliases
    /// while always returning the canonical configured model record.
    ///
    /// Later schema and picker layers depend on one provider-local identity
    /// contract instead of independently canonicalizing model names.
    #[test]
    fn provider_config_resolves_canonical_model_ids_and_aliases() {
        let provider = ProviderConfig {
            models: vec![ProviderModelConfig {
                id: "model-primary".to_string(),
                aliases: vec!["primary".to_string(), "fast".to_string()],
                context_window_tokens: Some(200_000),
                ..ProviderModelConfig::default()
            }],
            ..ProviderConfig::default()
        };

        assert_eq!(
            provider.model("model-primary").unwrap().unwrap().id,
            "model-primary"
        );
        let resolved = provider.model(" fast ").unwrap().unwrap();
        assert_eq!(resolved.id, "model-primary");
        assert_eq!(resolved.context_window_tokens, Some(200_000));
        assert!(provider.model("missing").unwrap().is_none());
    }

    /// Verifies duplicate canonical ids and aliases that overlap any provider
    /// model identity are rejected with stable typed error categories.
    ///
    /// Ambiguous aliases must be diagnosed before profiles, catalogs, or UI
    /// selection can resolve different effective models for the same input.
    #[test]
    fn provider_config_rejects_model_id_and_alias_collisions() {
        let duplicate_ids = ProviderConfig {
            models: vec![
                ProviderModelConfig::named("duplicate"),
                ProviderModelConfig::named(" duplicate "),
            ],
            ..ProviderConfig::default()
        };
        assert_eq!(
            duplicate_ids.validate_models().unwrap_err().kind(),
            ProviderModelConfigErrorKind::DuplicateId
        );

        let alias_collision = ProviderConfig {
            models: vec![
                ProviderModelConfig {
                    id: "model-a".to_string(),
                    aliases: vec!["shared".to_string()],
                    ..ProviderModelConfig::default()
                },
                ProviderModelConfig {
                    id: "shared".to_string(),
                    aliases: vec!["other".to_string()],
                    ..ProviderModelConfig::default()
                },
            ],
            ..ProviderConfig::default()
        };
        assert_eq!(
            alias_collision.validate_models().unwrap_err().kind(),
            ProviderModelConfigErrorKind::AliasCollision
        );
    }

    /// Verifies effective profiles inherit reusable provider-model metadata,
    /// canonicalize aliases, and retain explicit profile-level precedence.
    ///
    /// Root, model, and profile options merge per key while explicit empty
    /// list overrides remain distinguishable from omitted list metadata.
    #[test]
    fn provider_registry_materializes_profile_definitions_with_base_inheritance() {
        let provider = ProviderConfig {
            provider_id: "custom".to_string(),
            options: BTreeMap::from([
                ("root-only".to_string(), "root".to_string()),
                ("shared".to_string(), "root".to_string()),
            ]),
            models: vec![ProviderModelConfig {
                id: "model-a".to_string(),
                aliases: vec!["fast".to_string()],
                context_window_tokens: Some(200_000),
                max_input_tokens: Some(180_000),
                max_output_tokens: Some(8_000),
                reasoning_levels: Some(vec!["low".to_string(), "high".to_string()]),
                capabilities: Some(vec!["vision".to_string(), "tool_use".to_string()]),
                provider_options: BTreeMap::from([
                    ("base-only".to_string(), "base".to_string()),
                    ("shared".to_string(), "base".to_string()),
                ]),
                ..ProviderModelConfig::default()
            }],
            ..ProviderConfig::default()
        };
        let mut registry = ProviderRegistry {
            providers: BTreeMap::from([("custom".to_string(), provider)]),
            ..ProviderRegistry::default()
        };
        registry
            .insert_profile_definition(
                "work",
                ModelProfileDefinition {
                    provider: "custom".to_string(),
                    model: "fast".to_string(),
                    max_output_tokens: Some(16_000),
                    capabilities: Some(Vec::new()),
                    provider_options: BTreeMap::from([(
                        "shared".to_string(),
                        "profile".to_string(),
                    )]),
                    ..ModelProfileDefinition::default()
                },
                None,
            )
            .unwrap();

        let profile = registry.resolve_profile("work").unwrap();
        assert_eq!(profile.model, "model-a");
        assert_eq!(profile.context_window_tokens(), Some(200_000));
        assert_eq!(profile.max_input_tokens(), Some(180_000));
        assert_eq!(profile.max_output_tokens(), Some(16_000));
        assert_eq!(profile.provider_options["root-only"], "root");
        assert_eq!(profile.provider_options["base-only"], "base");
        assert_eq!(profile.provider_options["shared"], "profile");
        assert_eq!(
            profile.provider_options["model_reasoning_levels"],
            "low,high"
        );
        assert_eq!(profile.provider_options["model_capabilities"], "");
    }

    /// Verifies discovered metadata fills configured gaps for future profile
    /// resolutions while configured base values continue to win conflicts.
    ///
    /// Rematerialization replaces registry profiles only; previously cloned
    /// in-flight profiles remain unchanged values owned by their turns.
    #[test]
    fn provider_registry_rematerializes_profiles_from_catalog_observations() {
        let provider = ProviderConfig {
            provider_id: "custom".to_string(),
            models: vec![ProviderModelConfig {
                id: "model-a".to_string(),
                max_output_tokens: Some(16_000),
                ..ProviderModelConfig::default()
            }],
            ..ProviderConfig::default()
        };
        let mut registry = ProviderRegistry {
            providers: BTreeMap::from([("custom".to_string(), provider)]),
            ..ProviderRegistry::default()
        };
        registry
            .insert_profile_definition(
                "work",
                ModelProfileDefinition {
                    provider: "custom".to_string(),
                    model: "model-a".to_string(),
                    ..ModelProfileDefinition::default()
                },
                None,
            )
            .unwrap();
        let before = registry.resolve_profile("work").unwrap();

        let catalog = crate::ModelCatalog::from_input(crate::ModelCatalogInput {
            candidates: vec![crate::ModelCatalogCandidate::available(
                crate::ModelCatalogSource::Discovered,
                crate::ProviderModelInfo {
                    id: "model-a".to_string(),
                    display_name: None,
                    reasoning_levels: vec!["medium".to_string()],
                    context_window_tokens: Some(777_000),
                    max_input_tokens: Some(700_000),
                    max_output_tokens: Some(8_000),
                    capabilities: vec!["tool_use".to_string()],
                },
            )],
            ..crate::ModelCatalogInput::default()
        });
        registry
            .rematerialize_profiles_for_provider("custom", Some(&catalog))
            .unwrap();

        let after = registry.resolve_profile("work").unwrap();
        assert_eq!(before.known_context_window_tokens(), None);
        assert_eq!(after.known_context_window_tokens(), Some(777_000));
        assert_eq!(after.max_input_tokens(), Some(700_000));
        assert_eq!(after.max_output_tokens(), Some(16_000));
        assert_eq!(after.provider_options["model_capabilities"], "tool_use");
        assert_eq!(before.known_context_window_tokens(), None);
    }

    /// Verifies unlisted custom models remain materializable and impossible
    /// user-selected token limits remain authoritative for unlisted models.
    #[test]
    fn provider_registry_allows_unlisted_models_and_user_selected_limits() {
        let provider = ProviderConfig {
            provider_id: "custom".to_string(),
            ..ProviderConfig::default()
        };
        let mut registry = ProviderRegistry {
            providers: BTreeMap::from([("custom".to_string(), provider)]),
            ..ProviderRegistry::default()
        };
        registry
            .insert_profile_definition(
                "unlisted",
                ModelProfileDefinition {
                    provider: "custom".to_string(),
                    model: "external-model".to_string(),
                    ..ModelProfileDefinition::default()
                },
                None,
            )
            .unwrap();
        assert_eq!(
            registry.resolve_profile("unlisted").unwrap().model,
            "external-model"
        );

        registry
            .insert_profile_definition(
                "user-limits",
                ModelProfileDefinition {
                    provider: "custom".to_string(),
                    model: "external-model".to_string(),
                    context_window_tokens: Some(100),
                    max_input_tokens: Some(101),
                    ..ModelProfileDefinition::default()
                },
                None,
            )
            .unwrap();
        let profile = registry.resolve_profile("user-limits").unwrap();
        assert_eq!(profile.context_window_tokens(), Some(100));
        assert_eq!(profile.max_input_tokens(), Some(101));
    }

    /// Verifies preset lookup preserves configured profile identities.
    #[test]
    fn preset_registry_resolves_named_model_presets() {
        let preset = ModelPreset {
            default_model_profile: "medium".to_string(),
            auto_sizing_router_model_profile: "router".to_string(),
            auto_sizing_small_model_profile: "small".to_string(),
            auto_sizing_medium_model_profile: "medium".to_string(),
            auto_sizing_large_model_profile: "large".to_string(),
            allowed_reasoning_efforts: vec!["medium".to_string(), "high".to_string()],
        };
        let registry = PresetRegistry {
            presets: BTreeMap::from([("balanced".to_string(), preset)]),
        };
        assert!(registry.has_presets());
        assert_eq!(
            registry.resolve("balanced").unwrap().default_model_profile,
            "medium"
        );
    }
}
