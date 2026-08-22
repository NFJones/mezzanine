//! Configuration schema v67 to v68 migration.
//!
//! Schema v68 aligns the Iroh stream-limit setting with the version 1
//! transport contract, which owns exactly one client-opened bidirectional
//! control stream per connection.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Normalizes the Iroh control-stream limit and advances the schema version.
pub(super) fn migrate_v67_to_v68(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "transport.iroh.max_streams_per_connection",
                toml_edit::value(1),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(68))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "transport.iroh.max_streams_per_connection",
                serde_json::json!(1),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(68))?;
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
