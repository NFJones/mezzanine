//! Configuration schema v44 to v45 migration.
//!
//! Schema v45 removes inert configured trusted-directory and trusted-project
//! lists. Durable project trust remains exclusively managed by the project
//! trust store and its explicit commands.

use super::ops::{
    parse_json_compatible_config, remove_json_path, remove_toml_path, set_json_path_value,
    set_toml_path_item,
};
use super::{ConfigFormat, MezError, Result};

/// Removes inert permission trust-list settings and advances a v44 document.
pub(super) fn migrate_v44_to_v45(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            remove_toml_path(&mut document, "permissions.trusted_directories")?;
            remove_toml_path(&mut document, "permissions.trusted_projects")?;
            set_toml_path_item(&mut document, "version", toml_edit::value(45))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            remove_json_path(&mut document, "permissions.trusted_directories");
            remove_json_path(&mut document, "permissions.trusted_projects");
            set_json_path_value(&mut document, "version", serde_json::json!(45))?;
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
