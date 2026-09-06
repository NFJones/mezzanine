//! Configuration schema v88 to v89 migration.
//!
//! Schema v89 adds pane-status presets. Existing explicit rails and pill
//! definitions remain authoritative, while `standard` preserves the previous
//! omitted-value behavior.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the standard pane-status preset without changing existing appearance.
pub(super) fn migrate_v88_to_v89(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            if super::ops::toml_item_at(document.as_table(), "frames.pane.status_preset").is_none()
            {
                set_toml_path_item(
                    &mut document,
                    "frames.pane.status_preset",
                    toml_edit::value("standard"),
                )?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(89))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            if super::ops::json_value_at(&document, "frames.pane.status_preset").is_none() {
                set_json_path_value(
                    &mut document,
                    "frames.pane.status_preset",
                    serde_json::json!("standard"),
                )?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(89))?;
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
