//! Runtime provider registry and model preset records.
//!
//! This module owns the data-only runtime records that describe configured
//! model providers and resolved model presets. Keeping these records outside
//! the central runtime service state makes provider configuration ownership
//! explicit while preserving the existing runtime facade API.

use super::{BTreeMap, MezError, ModelProfile, Result};

/// Carries Runtime Provider Registry state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeProviderRegistry {
    /// Stores the default profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) default_profile: Option<String>,
    /// Stores the providers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) providers: BTreeMap<String, RuntimeProviderConfig>,
    /// Stores the profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) profiles: BTreeMap<String, ModelProfile>,
    /// Stores the fallback profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) fallback_profiles: BTreeMap<String, Vec<String>>,
}

/// Carries Runtime Provider Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProviderConfig {
    /// Stores the provider id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider_id: String,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: String,
    /// Stores the optional API compatibility selector for this provider.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub api: Option<String>,
    /// Stores the auth profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub auth_profile: String,
    /// Stores the base url value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub base_url: Option<String>,
    /// Stores the models value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub models: Vec<String>,
    /// Stores the default model value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub default_model: Option<String>,
    /// Stores the options value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub options: BTreeMap<String, String>,
}

impl RuntimeProviderRegistry {
    /// Runs the default profile name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn default_profile_name(&self) -> Option<&str> {
        self.default_profile.as_deref()
    }

    /// Runs the provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider(&self, provider_id: &str) -> Option<&RuntimeProviderConfig> {
        self.providers.get(provider_id)
    }

    /// Runs the profile operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn profile(&self, profile_name: &str) -> Option<&ModelProfile> {
        self.profiles.get(profile_name)
    }

    /// Runs the resolve profile operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resolve_profile(&self, profile_name: &str) -> Result<ModelProfile> {
        self.profile(profile_name).cloned().ok_or_else(|| {
            MezError::config(format!("model profile `{profile_name}` is not configured"))
        })
    }

    /// Runs the providers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn providers(&self) -> &BTreeMap<String, RuntimeProviderConfig> {
        &self.providers
    }

    /// Runs the profiles operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn profiles(&self) -> &BTreeMap<String, ModelProfile> {
        &self.profiles
    }

    /// Runs the safe fallback profiles operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn safe_fallback_profiles(&self, profile_name: &str) -> Result<Vec<String>> {
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

/// Carries Runtime Model Preset state for this subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModelPreset {
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

/// Carries Runtime Preset Registry state for this subsystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimePresetRegistry {
    /// Named model presets keyed by preset identity.
    pub presets: BTreeMap<String, RuntimeModelPreset>,
}

impl RuntimePresetRegistry {
    /// Returns true when at least one preset is defined.
    pub fn has_presets(&self) -> bool {
        !self.presets.is_empty()
    }

    /// Resolves a preset by name.
    pub fn resolve(&self, name: &str) -> Option<&RuntimeModelPreset> {
        self.presets.get(name)
    }
}
