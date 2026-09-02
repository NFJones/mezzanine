//! Configuration schema v80 to v81 migration.
//!
//! Schema v81 makes provider-error retries configurable and adds a separate
//! unlimited mode. Existing configurations retain the historical five-retry
//! finite policy unless the user explicitly opts into unlimited retries.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

/// Provider retry policy paths introduced by schema v81.
const PROVIDER_RETRY_DEFAULT_PATHS: &[&str] = &[
    "agents.provider_error_retry_limit",
    "agents.provider_error_retry_unlimited",
];

/// Adds provider retry policy defaults and advances the document to v81.
pub(super) fn migrate_v80_to_v81(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let defaults = DEFAULT_CONFIG_TOML
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| {
                    MezError::config(format!("invalid default TOML config: {error}"))
                })?;
            for path in PROVIDER_RETRY_DEFAULT_PATHS {
                copy_toml_default_if_absent(&mut document, &defaults, path)?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(81))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            let defaults = toml::from_str::<toml::Value>(DEFAULT_CONFIG_TOML).map_err(|error| {
                MezError::config(format!("invalid default TOML config: {error}"))
            })?;
            let defaults = serde_json::to_value(defaults).map_err(|error| {
                MezError::config(format!("failed to convert default config: {error}"))
            })?;
            for path in PROVIDER_RETRY_DEFAULT_PATHS {
                copy_json_default_if_absent(&mut document, &defaults, path)?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(81))?;
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
