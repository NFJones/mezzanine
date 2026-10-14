//! Schema 102 to 103 adds bounded presentation-only clock format selection.
//!
//! This version-only migration preserves authored formats, enablement choices
//! and neighboring policy in every encoding. Absence inherits HH:MM:SS without
//! enabling timestamps; validation, rather than migration, judges authored values.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Advances version authority without normalizing or materializing a format.
pub(super) fn migrate_v102_to_v103(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(&mut document, "version", toml_edit::value(103))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(&mut document, "version", serde_json::json!(103))?;
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
