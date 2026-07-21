//! Configuration schema v21 to v22 migration.
//!
//! Schema v22 introduces an explicit root-turn application policy for
//! auto-sizing decisions. Existing root turns used the routed-subagent path,
//! so migration materializes that behavior as the `subagent` default.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

/// Migrates schema v21 configuration by materializing the root routing policy.
pub(super) fn migrate_v21_to_v22(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v21_to_v22(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_v21_to_v22(format, text),
    }
}

/// Adds the default root routing policy to a TOML document and advances its version.
fn migrate_toml_v21_to_v22(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    let defaults = DEFAULT_CONFIG_TOML
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid built-in TOML config: {error}")))?;
    copy_toml_default_if_absent(
        &mut document,
        &defaults,
        "agents.auto_sizing.root_routing_policy",
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(22))?;
    Ok(document.to_string())
}

/// Adds the default root routing policy to JSON-compatible documents and advances their version.
fn migrate_json_v21_to_v22(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    let default_table = toml::from_str::<toml::Table>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    let defaults = serde_json::to_value(default_table)
        .map_err(|error| MezError::config(format!("invalid built-in default config: {error}")))?;
    copy_json_default_if_absent(
        &mut document,
        &defaults,
        "agents.auto_sizing.root_routing_policy",
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(22))?;
    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(&document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(&document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}
