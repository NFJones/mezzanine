//! Local plugin installation and removal.
//!
//! Installation copies an already-present local plugin package into the
//! Mezzanine plugin store. It validates the manifest first and never executes
//! plugin-provided files.

use super::manifest::PluginManifest;
use super::registry::{InstalledPlugin, PluginRegistry};
use crate::{MezError, Result};
use std::path::{Path, PathBuf};

/// Returns the local installed package root for one plugin id.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `plugin_id`: Valid plugin id.
pub fn installed_plugin_root(config_root: &Path, plugin_id: &str) -> PathBuf {
    config_root
        .join("plugins")
        .join("installed")
        .join(plugin_id)
}

/// Installs a local plugin package and records it disabled or enabled.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `source`: Local plugin package root.
/// - `enabled`: Whether the installed plugin should be active immediately.
pub fn install_local_plugin(config_root: &Path, source: &Path, enabled: bool) -> Result<String> {
    let source = source.canonicalize().map_err(|error| {
        MezError::new(
            crate::MezErrorKind::Io,
            format!(
                "failed to resolve plugin source {}: {error}",
                source.display()
            ),
        )
    })?;
    if !source.is_dir() {
        return Err(MezError::invalid_args(format!(
            "plugin source {} is not a directory",
            source.display()
        )));
    }
    let manifest = PluginManifest::read_from_root(&source)?;
    let mut registry = PluginRegistry::read(config_root)?;
    if registry.plugins.contains_key(&manifest.id) {
        return Err(MezError::conflict(format!(
            "plugin {:?} is already installed",
            manifest.id
        )));
    }
    let destination = installed_plugin_root(config_root, &manifest.id);
    if destination.exists() {
        std::fs::remove_dir_all(&destination).map_err(|error| {
            MezError::new(
                crate::MezErrorKind::Io,
                format!(
                    "failed to remove stale plugin directory {}: {error}",
                    destination.display()
                ),
            )
        })?;
    }
    copy_directory(&source, &destination)?;
    registry.plugins.insert(
        manifest.id.clone(),
        InstalledPlugin {
            id: manifest.id.clone(),
            name: manifest.name,
            description: manifest.description,
            version: manifest.version,
            path: destination,
            enabled,
        },
    );
    registry.write(config_root)?;
    Ok(format!(
        "installed plugin {} enabled={enabled}",
        manifest.id
    ))
}

/// Removes one installed plugin and its copied package directory.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `plugin_id`: Installed plugin id.
pub fn uninstall_plugin(config_root: &Path, plugin_id: &str) -> Result<String> {
    let mut registry = PluginRegistry::read(config_root)?;
    let removed = registry.plugins.remove(plugin_id).ok_or_else(|| {
        MezError::new(
            crate::MezErrorKind::NotFound,
            format!("plugin {plugin_id:?} is not installed"),
        )
    })?;
    if removed.path.exists() {
        std::fs::remove_dir_all(&removed.path).map_err(|error| {
            MezError::new(
                crate::MezErrorKind::Io,
                format!(
                    "failed to remove plugin directory {}: {error}",
                    removed.path.display()
                ),
            )
        })?;
    }
    registry.write(config_root)?;
    Ok(format!("uninstalled plugin {plugin_id}"))
}

/// Updates installed plugin enablement state.
///
/// # Parameters
/// - `config_root`: Primary Mezzanine configuration root.
/// - `plugin_id`: Installed plugin id.
/// - `enabled`: New enablement state.
pub fn set_plugin_enabled(config_root: &Path, plugin_id: &str, enabled: bool) -> Result<String> {
    let mut registry = PluginRegistry::read(config_root)?;
    let plugin = registry.plugins.get_mut(plugin_id).ok_or_else(|| {
        MezError::new(
            crate::MezErrorKind::NotFound,
            format!("plugin {plugin_id:?} is not installed"),
        )
    })?;
    plugin.enabled = enabled;
    registry.write(config_root)?;
    Ok(format!("plugin {plugin_id} enabled={enabled}"))
}

/// Recursively copies one directory while refusing symlink entries.
fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination).map_err(|error| {
        MezError::new(
            crate::MezErrorKind::Io,
            format!(
                "failed to create plugin directory {}: {error}",
                destination.display()
            ),
        )
    })?;
    let entries = std::fs::read_dir(source).map_err(|error| {
        MezError::new(
            crate::MezErrorKind::Io,
            format!("failed to list plugin source {}: {error}", source.display()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            MezError::new(
                crate::MezErrorKind::Io,
                format!("failed to read entry: {error}"),
            )
        })?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path).map_err(|error| {
            MezError::new(
                crate::MezErrorKind::Io,
                format!(
                    "failed to inspect plugin entry {}: {error}",
                    source_path.display()
                ),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(MezError::config(format!(
                "plugin entry {} must not be a symlink",
                source_path.display()
            )));
        }
        if metadata.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &destination_path).map_err(|error| {
                MezError::new(
                    crate::MezErrorKind::Io,
                    format!(
                        "failed to copy plugin file {} to {}: {error}",
                        source_path.display(),
                        destination_path.display()
                    ),
                )
            })?;
        }
    }
    Ok(())
}
