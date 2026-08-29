//! Provider profile registry and model-preset routing policy.
//!
//! This module owns provider-independent configured records, profile lookup,
//! failover safety filtering, and preset resolution. Product configuration
//! parsing, credentials, transport construction, and provider invocation stay
//! in the root package.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use crate::ModelProfile;

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
