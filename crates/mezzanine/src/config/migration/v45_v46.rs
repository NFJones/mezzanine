//! Configuration schema v45 to v46 migration.
//!
//! Schema v46 makes completion-attention title-pill flashing configurable.
//! Existing configurations retain the established enabled behavior explicitly.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the default-enabled completion-attention flashing setting to v45 configs.
pub(super) fn migrate_v45_to_v46(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "terminal.completion_attention_flashing",
                toml_edit::value(true),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(46))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "terminal.completion_attention_flashing",
                serde_json::json!(true),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(46))?;
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
