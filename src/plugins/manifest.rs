//! Plugin manifest parsing and validation.
//!
//! A plugin manifest is untrusted declarative metadata. This module validates
//! identity fields and relative payload paths without executing or importing
//! any plugin-provided content.

use crate::{MezError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// File name used for Mezzanine plugin manifests.
pub const PLUGIN_MANIFEST_FILE_NAME: &str = "mez-plugin.toml";

/// Version supported by the v1 plugin manifest parser.
const SUPPORTED_PLUGIN_SCHEMA_VERSION: u32 = 1;

/// Declarative Mezzanine plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Stable path-safe plugin identifier.
    pub id: String,
    /// Human-facing plugin name.
    pub name: String,
    /// Human-facing plugin description.
    pub description: String,
    /// Human-facing plugin version string.
    pub version: String,
    /// Optional runtime payload locations.
    #[serde(default)]
    pub payloads: PluginPayloads,
}

/// Relative runtime payload paths declared by a plugin manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPayloads {
    /// Relative directory containing one child directory per skill.
    pub skills: Option<PathBuf>,
    /// Reserved relative MCP payload path.
    pub mcp_servers: Option<PathBuf>,
    /// Reserved relative hook payload path.
    pub hooks: Option<PathBuf>,
    /// Reserved relative subagent payload path.
    pub subagents: Option<PathBuf>,
    /// Reserved relative personality payload path.
    pub personalities: Option<PathBuf>,
}

impl PluginManifest {
    /// Parses and validates one plugin manifest from TOML text.
    ///
    /// # Parameters
    /// - `text`: Complete `mez-plugin.toml` contents.
    pub fn parse(text: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(text)
            .map_err(|error| MezError::config(format!("invalid plugin manifest TOML: {error}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reads, parses, and validates a manifest from a plugin root.
    ///
    /// # Parameters
    /// - `root`: Plugin package root containing `mez-plugin.toml`.
    pub fn read_from_root(root: &Path) -> Result<Self> {
        let path = root.join(PLUGIN_MANIFEST_FILE_NAME);
        let text = std::fs::read_to_string(&path).map_err(|error| {
            MezError::new(
                crate::MezErrorKind::Io,
                format!("failed to read plugin manifest {}: {error}", path.display()),
            )
        })?;
        Self::parse(&text)
    }

    /// Validates manifest invariants that are independent of installation.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_PLUGIN_SCHEMA_VERSION {
            return Err(MezError::config(format!(
                "unsupported plugin schema_version {}; expected {SUPPORTED_PLUGIN_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if !is_valid_plugin_id(&self.id) {
            return Err(MezError::config(format!(
                "plugin id {:?} is invalid; use lowercase letters, digits, and hyphens",
                self.id
            )));
        }
        if self.name.trim().is_empty() {
            return Err(MezError::config("plugin name must not be empty"));
        }
        if self.description.trim().is_empty() {
            return Err(MezError::config("plugin description must not be empty"));
        }
        self.payloads.validate()?;
        Ok(())
    }
}

impl PluginPayloads {
    /// Validates that all declared payload paths are relative and contained.
    pub fn validate(&self) -> Result<()> {
        for (label, path) in [
            ("skills", self.skills.as_ref()),
            ("mcp_servers", self.mcp_servers.as_ref()),
            ("hooks", self.hooks.as_ref()),
            ("subagents", self.subagents.as_ref()),
            ("personalities", self.personalities.as_ref()),
        ] {
            if let Some(path) = path {
                validate_relative_payload_path(label, path)?;
            }
        }
        Ok(())
    }
}

/// Returns whether a candidate plugin identifier is path-safe.
///
/// # Parameters
/// - `id`: Candidate plugin identifier.
pub fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && id
            .bytes()
            .any(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// Validates one manifest payload path.
fn validate_relative_payload_path(label: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(MezError::config(format!(
            "plugin payload path {label} must be relative"
        )));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => {
                return Err(MezError::config(format!(
                    "plugin payload path {label} must not escape the plugin root"
                )));
            }
        }
    }
    Ok(())
}
