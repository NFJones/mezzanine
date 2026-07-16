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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Configured model names.
    pub models: Vec<String>,
    /// Optional default model name.
    pub default_model: Option<String>,
    /// Secret-free provider options used by request policy.
    pub options: BTreeMap<String, String>,
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
