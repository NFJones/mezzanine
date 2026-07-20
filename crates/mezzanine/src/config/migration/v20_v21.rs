//! Configuration schema v20 to v21 migration.
//!
//! Schema v21 introduces optional resource authority and sandbox configuration.
//! Migration preserves historical authorization behavior by selecting the
//! policy-only backend and deliberately does not infer filesystem scopes or
//! command effects from existing rules, presets, approvals, or trust state.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Migrates schema v20 configuration without inventing sandbox authority.
pub(super) fn migrate_v20_to_v21(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => migrate_toml_v20_to_v21(text),
        ConfigFormat::Yaml | ConfigFormat::Json => migrate_json_v20_to_v21(format, text),
    }
}

fn migrate_toml_v20_to_v21(text: &str) -> Result<String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
    set_toml_path_item(
        &mut document,
        "permissions.sandbox",
        toml_edit::value("policy-only"),
    )?;
    set_toml_path_item(&mut document, "version", toml_edit::value(21))?;
    Ok(document.to_string())
}

fn migrate_json_v20_to_v21(format: ConfigFormat, text: &str) -> Result<String> {
    let mut document = parse_json_compatible_config(format, text)?;
    set_json_path_value(
        &mut document,
        "permissions.sandbox",
        serde_json::json!("policy-only"),
    )?;
    set_json_path_value(&mut document, "version", serde_json::json!(21))?;
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
