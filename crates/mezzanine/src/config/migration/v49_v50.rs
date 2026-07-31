//! Configuration schema v49 to v50 migration.
//!
//! Schema v50 adds an empty primary-user Bubblewrap environment-variable
//! whitelist. Existing configurations retain the fixed minimal environment.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the empty pane environment whitelist and advances to v50.
pub(super) fn migrate_v49_to_v50(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "permissions.bubblewrap.env_whitelist",
                toml_edit::value(toml_edit::Array::new()),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(50))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "permissions.bubblewrap.env_whitelist",
                serde_json::json!([]),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(50))?;
            match format {
                ConfigFormat::Json => serde_json::to_string_pretty(&document)
                    .map(|mut rendered| {
                        rendered.push(char::from(10));
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
