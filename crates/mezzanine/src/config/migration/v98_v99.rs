//! Configuration schema v98 to v99 migration.
//!
//! Discovery is presence-preserving and defaults off. Advancing the version
//! must not materialize a kill switch or opt-in, or alter authored action lists.

use super::ops::{set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Advances only the document version; all authored policy remains unchanged.
pub(super) fn migrate_v98_to_v99(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(&mut document, "version", toml_edit::value(99))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = super::ops::parse_json_compatible_config(format, text)?;
            set_json_path_value(&mut document, "version", serde_json::json!(99))?;
            match format {
                ConfigFormat::Json => serde_json::to_string_pretty(&document)
                    .map(|rendered| format!("{rendered}\n"))
                    .map_err(|error| {
                        MezError::config(format!("failed to render JSON config: {error}"))
                    }),
                ConfigFormat::Yaml => serde_norway::to_string(&document).map_err(|error| {
                    MezError::config(format!("failed to render YAML config: {error}"))
                }),
                ConfigFormat::Toml => unreachable!("TOML handled above"),
            }
        }
    }
}
