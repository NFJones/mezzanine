//! Configuration schema v78 to v79 migration.
//!
//! Schema v79 adds portable structured external-editor command candidates.
//! Existing key-preset selection and prompt behavior remain unchanged.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

const EXTERNAL_EDITOR_DEFAULT_PATHS: &[&str] =
    &["external_editor.command", "external_editor.fallback"];

/// Adds structured editor candidates and advances the document to schema v79.
pub(super) fn migrate_v78_to_v79(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let defaults = DEFAULT_CONFIG_TOML
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| {
                    MezError::config(format!("invalid default TOML config: {error}"))
                })?;
            for path in EXTERNAL_EDITOR_DEFAULT_PATHS {
                copy_toml_default_if_absent(&mut document, &defaults, path)?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(79))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            let defaults = toml::from_str::<toml::Value>(DEFAULT_CONFIG_TOML).map_err(|error| {
                MezError::config(format!("invalid default TOML config: {error}"))
            })?;
            let defaults = serde_json::to_value(defaults).map_err(|error| {
                MezError::config(format!("failed to convert default config: {error}"))
            })?;
            for path in EXTERNAL_EDITOR_DEFAULT_PATHS {
                copy_json_default_if_absent(&mut document, &defaults, path)?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(79))?;
            match format {
                ConfigFormat::Json => serde_json::to_string_pretty(&document)
                    .map(|mut rendered| {
                        rendered.push('\n');
                        rendered
                    })
                    .map_err(|error| {
                        MezError::config(format!("failed to render JSON config: {error}"))
                    }),
                ConfigFormat::Yaml => serde_norway::to_string(&document).map_err(|error| {
                    MezError::config(format!("failed to render YAML config: {error}"))
                }),
                ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
            }
        }
    }
}
