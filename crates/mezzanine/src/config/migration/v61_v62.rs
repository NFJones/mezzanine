//! Configuration schema v61 to v62 migration.
//!
//! Schema v62 makes scheduler queue admission finite. Existing configurations
//! receive the new queued-turn count and estimated-byte defaults so upgraded
//! behavior remains deterministic across all supported configuration formats.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds bounded agent scheduler queue settings and advances the schema version.
pub(super) fn migrate_v61_to_v62(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "agents.max_queued_turns",
                toml_edit::value(256),
            )?;
            set_toml_path_item(
                &mut document,
                "agents.max_queued_bytes",
                toml_edit::value(4_194_304),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(62))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "agents.max_queued_turns",
                serde_json::json!(256),
            )?;
            set_json_path_value(
                &mut document,
                "agents.max_queued_bytes",
                serde_json::json!(4_194_304),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(62))?;
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
