//! Configuration schema 101 to 102: optional structured-log timestamp display.
//!
//! Absence inherits the new default false; authored true/false values and all
//! neighboring terminal settings remain untouched. This version-only migration
//! preserves presence semantics in TOML (including inline tables), YAML and JSON
//! rather than silently enabling decoration during recovery.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Advances schema authority while preserving every authored display choice.
pub(super) fn migrate_v101_to_v102(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(&mut document, "version", toml_edit::value(102))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(&mut document, "version", serde_json::json!(102))?;
            match format {
                ConfigFormat::Json => serde_json::to_string_pretty(&document)
                    .map(|rendered| format!("{rendered}\n"))
                    .map_err(|error| {
                        MezError::config(format!("failed to render JSON config: {error}"))
                    }),
                ConfigFormat::Yaml => serde_norway::to_string(&document).map_err(|error| {
                    MezError::config(format!("failed to render YAML config: {error}"))
                }),
                ConfigFormat::Toml => unreachable!("TOML migration handled above"),
            }
        }
    }
}
