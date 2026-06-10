//! Installed plugin registry persistence.
//!
//! The registry records installed plugin roots and enablement under the primary
//! Mezzanine configuration root. It is the authoritative local state for v1
//! plugin loading and deliberately stays separate from ordinary config schema
//! mutation so plugin package state can evolve independently.

use crate::{MezError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Installed plugin registry persisted as TOML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRegistry {
    /// Installed plugin entries keyed by plugin id.
    #[serde(default)]
    pub plugins: BTreeMap<String, InstalledPlugin>,
}

/// One installed plugin entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPlugin {
    /// Stable plugin id.
    pub id: String,
    /// Plugin display name recorded at install time.
    pub name: String,
    /// Plugin description recorded at install time.
    pub description: String,
    /// Plugin version recorded at install time.
    pub version: String,
    /// Local installed package root.
    pub path: PathBuf,
    /// Whether runtime payloads are active.
    pub enabled: bool,
}

/// Returns the registry file path for one config root.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
pub fn plugin_registry_path(config_root: &Path) -> PathBuf {
    config_root.join("plugins").join("installed.toml")
}

impl PluginRegistry {
    /// Reads the installed plugin registry, returning an empty registry when it
    /// does not exist.
    ///
    /// # Parameters
    /// - `config_root`: Primary Mezzanine configuration root.
    pub fn read(config_root: &Path) -> Result<Self> {
        let path = plugin_registry_path(config_root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(MezError::new(
                    crate::MezErrorKind::Io,
                    format!("failed to read plugin registry {}: {error}", path.display()),
                ));
            }
        };
        toml::from_str(&text).map_err(|error| {
            MezError::config(format!(
                "failed to parse plugin registry {}: {error}",
                path.display()
            ))
        })
    }

    /// Writes the installed plugin registry using private config-directory
    /// permissions inherited from the parent tree.
    ///
    /// # Parameters
    /// - `config_root`: Primary Mezzanine configuration root.
    pub fn write(&self, config_root: &Path) -> Result<()> {
        let path = plugin_registry_path(config_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                MezError::new(
                    crate::MezErrorKind::Io,
                    format!("failed to create plugin registry directory: {error}"),
                )
            })?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| {
            MezError::config(format!("failed to encode plugin registry: {error}"))
        })?;
        std::fs::write(&path, text).map_err(|error| {
            MezError::new(
                crate::MezErrorKind::Io,
                format!(
                    "failed to write plugin registry {}: {error}",
                    path.display()
                ),
            )
        })
    }
}
