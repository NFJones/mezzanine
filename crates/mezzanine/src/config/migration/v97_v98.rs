//! Configuration schema v97 to v98 migration.
//!
//! Schema v98 removes the obsolete remote-lease startup recovery policy.

use super::ops::{remove_json_path, remove_toml_path, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Removes `host.recover_on_start` without changing the other host settings.
pub(super) fn migrate_v97_to_v98(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            remove_toml_path(&mut document, "host.recover_on_start")?;
            set_toml_path_item(&mut document, "version", toml_edit::value(98))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = super::ops::parse_json_compatible_config(format, text)?;
            remove_json_path(&mut document, "host.recover_on_start");
            set_json_path_value(&mut document, "version", serde_json::json!(98))?;
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
