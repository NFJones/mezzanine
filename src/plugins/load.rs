//! Side-effect-free plugin runtime loading.
//!
//! Loading reads enabled installed plugin manifests and produces capability
//! overlays that other subsystems can consume. The v1 overlay activates plugin
//! skill roots only; reserved payloads are surfaced as diagnostics until their
//! runtime integrations are implemented.

use super::manifest::PluginManifest;
use super::registry::PluginRegistry;
use std::path::{Path, PathBuf};

/// Runtime load outcome for enabled plugins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginLoadOutcome {
    /// Enabled plugin skill roots.
    pub skill_roots: Vec<PluginSkillRoot>,
    /// Non-fatal load diagnostics.
    pub diagnostics: Vec<String>,
}

/// One enabled plugin skill root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSkillRoot {
    /// Plugin id that owns this skill root.
    pub plugin_id: String,
    /// Absolute skill root path.
    pub path: PathBuf,
}

/// Loads enabled plugin payloads from the installed registry.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
pub fn load_enabled_plugins(config_root: &Path) -> PluginLoadOutcome {
    let mut outcome = PluginLoadOutcome::default();
    let registry = match PluginRegistry::read(config_root) {
        Ok(registry) => registry,
        Err(error) => {
            outcome.diagnostics.push(format!(
                "plugin registry could not be loaded: {}",
                error.message()
            ));
            return outcome;
        }
    };
    for plugin in registry.plugins.values().filter(|plugin| plugin.enabled) {
        let manifest = match PluginManifest::read_from_root(&plugin.path) {
            Ok(manifest) => manifest,
            Err(error) => {
                outcome.diagnostics.push(format!(
                    "plugin {} manifest could not be loaded: {}",
                    plugin.id,
                    error.message()
                ));
                continue;
            }
        };
        if manifest.id != plugin.id {
            outcome.diagnostics.push(format!(
                "plugin {} manifest id changed to {}; skipping",
                plugin.id, manifest.id
            ));
            continue;
        }
        if let Some(skills) = manifest.payloads.skills {
            outcome.skill_roots.push(PluginSkillRoot {
                plugin_id: plugin.id.clone(),
                path: plugin.path.join(skills),
            });
        }
        if manifest.payloads.mcp_servers.is_some() {
            outcome.diagnostics.push(format!(
                "plugin {} declares MCP servers; plugin MCP activation is reserved for a later phase",
                plugin.id
            ));
        }
        if manifest.payloads.hooks.is_some() {
            outcome.diagnostics.push(format!(
                "plugin {} declares hooks; plugin hook activation is reserved for a later phase",
                plugin.id
            ));
        }
        if manifest.payloads.subagents.is_some() {
            outcome.diagnostics.push(format!(
                "plugin {} declares subagents; plugin subagent activation is reserved for a later phase",
                plugin.id
            ));
        }
        if manifest.payloads.personalities.is_some() {
            outcome.diagnostics.push(format!(
                "plugin {} declares personalities; plugin personality activation is reserved for a later phase",
                plugin.id
            ));
        }
    }
    outcome
}
