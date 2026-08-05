//! Configuration schema v50 to v51 migration.
//!
//! Schema v51 removes Bubblewrap toolchain selectors and custom definitions.
//! Generic read/write scopes and environment forwarding remain available.

use super::ops::{
    parse_json_compatible_config, remove_json_path, remove_toml_path, set_json_path_value,
    set_toml_path_item,
};
use super::{ConfigFormat, MezError, Result};

/// Removes toolchain configuration and advances a v50 document.
pub(super) fn migrate_v50_to_v51(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            remove_toml_path(&mut document, "permissions.bubblewrap.toolchains")?;
            remove_toml_path(&mut document, "permissions.bubblewrap.custom_toolchains")?;
            set_toml_path_item(&mut document, "version", toml_edit::value(51))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            remove_json_path(&mut document, "permissions.bubblewrap.toolchains");
            remove_json_path(&mut document, "permissions.bubblewrap.custom_toolchains");
            set_json_path_value(&mut document, "version", serde_json::json!(51))?;
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
