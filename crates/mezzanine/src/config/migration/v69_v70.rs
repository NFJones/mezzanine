//! Configuration schema v69 to v70 migration.
//!
//! Schema v70 adds an explicit Iroh bind port. Existing configurations retain
//! ephemeral binding until the primary user selects a stable direct port.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the default ephemeral Iroh bind port and advances the schema version.
pub(super) fn migrate_v69_to_v70(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "transport.iroh.bind_port",
                toml_edit::value(0),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(70))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "transport.iroh.bind_port",
                serde_json::json!(0),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(70))?;
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
