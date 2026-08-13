//! Configuration schema v60 to v61 migration.
//!
//! Schema v61 makes host clipboard acquisition limits explicit. Existing
//! configurations receive the prior effective deadline and the new finite
//! one-mebibyte payload ceiling so upgraded behavior is deterministic.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds bounded host clipboard read settings and advances the schema version.
pub(super) fn migrate_v60_to_v61(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "terminal.clipboard_read_timeout_ms",
                toml_edit::value(250),
            )?;
            set_toml_path_item(
                &mut document,
                "terminal.clipboard_read_max_bytes",
                toml_edit::value(1_048_576),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(61))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "terminal.clipboard_read_timeout_ms",
                serde_json::json!(250),
            )?;
            set_json_path_value(
                &mut document,
                "terminal.clipboard_read_max_bytes",
                serde_json::json!(1_048_576),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(61))?;
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
