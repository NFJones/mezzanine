//! Configuration schema v68 to v69 migration.
//!
//! Schema v69 separates explicit outbound Iroh permission from inbound
//! listener enablement. Existing configurations retain explicit outbound
//! profile and invitation use unless the owner later opts out.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds explicit outbound Iroh permission and advances the schema version.
pub(super) fn migrate_v68_to_v69(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "transport.iroh.outbound_enabled",
                toml_edit::value(true),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(69))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "transport.iroh.outbound_enabled",
                serde_json::json!(true),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(69))?;
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
