//! Configuration schema v77 to v78 migration.
//!
//! Schema v78 establishes time-and-count saved-session retention. Migration
//! replaces only the historical built-in count default and materializes the
//! age default while preserving every explicit custom count.

use super::ops::{
    parse_json_compatible_config, set_json_default_usize_if_absent_or_old_default,
    set_json_path_value, set_toml_default_usize_if_absent_or_old_default, set_toml_path_item,
};
use super::{ConfigFormat, MezError, Result};

/// Adds saved-session age retention and advances the document to schema v78.
pub(super) fn migrate_v77_to_v78(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_default_usize_if_absent_or_old_default(
                &mut document,
                "history.saved_sessions_limit",
                100,
                10_000,
            )?;
            set_toml_default_usize_if_absent_or_old_default(
                &mut document,
                "history.saved_sessions_retention_days",
                90,
                90,
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(78))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_default_usize_if_absent_or_old_default(
                &mut document,
                "history.saved_sessions_limit",
                100,
                10_000,
            )?;
            set_json_default_usize_if_absent_or_old_default(
                &mut document,
                "history.saved_sessions_retention_days",
                90,
                90,
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(78))?;
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
