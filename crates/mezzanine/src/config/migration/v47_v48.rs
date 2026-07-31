//! Configuration schema v47 to v48 migration.
//!
//! Schema v48 replaces ambient supplementary-group inheritance with an exact,
//! primary-user-selected Bubblewrap group list. Existing configurations receive
//! an empty list so upgrades do not silently retain ambient group authority.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the empty supplementary-group mapping to v47 configurations.
pub(super) fn migrate_v47_to_v48(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "permissions.bubblewrap.supplementary_groups",
                toml_edit::value(toml_edit::Array::new()),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(48))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "permissions.bubblewrap.supplementary_groups",
                serde_json::json!([]),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(48))?;
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
