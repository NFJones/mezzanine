//! Schema v90 adds the bounded zen focus-label duration.
//!
//! Preserve explicit values in every durable format; normal validation remains
//! responsible for rejecting invalid values after migration.

use super::ops::{
    copy_json_default_if_absent, copy_toml_default_if_absent, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

/// Adds the duration default without replacing user choices and advances v89.
pub(super) fn migrate_v89_to_v90(format: ConfigFormat, text: &str) -> Result<String> {
    let defaults = toml::from_str::<toml::Value>(DEFAULT_CONFIG_TOML)
        .map_err(|error| MezError::config(format!("invalid default TOML config: {error}")))?;
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
            copy_toml_default_if_absent(
                &mut document,
                &defaults,
                "terminal.zen_focus_label_duration_ms",
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(90))?;
            Ok(document.to_string())
        }
        ConfigFormat::Json | ConfigFormat::Yaml => {
            let mut document = parse_json_compatible_config(format, text)?;
            let defaults = serde_json::to_value(defaults).map_err(|error| {
                MezError::config(format!("failed to convert default config: {error}"))
            })?;
            copy_json_default_if_absent(
                &mut document,
                &defaults,
                "terminal.zen_focus_label_duration_ms",
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(90))?;
            if format == ConfigFormat::Json {
                serde_json::to_string_pretty(&document)
                    .map(|text| text + "\n")
                    .map_err(|error| {
                        MezError::config(format!("failed to render JSON config: {error}"))
                    })
            } else {
                serde_norway::to_string(&document).map_err(|error| {
                    MezError::config(format!("failed to render YAML config: {error}"))
                })
            }
        }
    }
}
