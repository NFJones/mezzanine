//! Configuration schema v91 to v92 migration.
//!
//! Schema v92 removes the inert `frames.window.pills.<name>.style` leaf.
//! Window pills retain their existing semantic rendition and explicit palette
//! overrides; pane-pill style selectors remain supported.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Removes inert window-pill style leaves and advances a v91 document.
pub(super) fn migrate_v91_to_v92(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            if let Some(pills) = document
                .as_table_mut()
                .get_mut("frames")
                .and_then(toml_edit::Item::as_table_mut)
                .and_then(|frames| frames.get_mut("window"))
                .and_then(toml_edit::Item::as_table_mut)
                .and_then(|window| window.get_mut("pills"))
                .and_then(toml_edit::Item::as_table_mut)
            {
                for (_name, pill) in pills.iter_mut() {
                    if let Some(pill) = pill.as_table_mut() {
                        pill.remove("style");
                    } else if let Some(pill) = pill.as_inline_table_mut() {
                        pill.remove("style");
                    }
                }
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(92))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            if let Some(pills) = document
                .pointer_mut("/frames/window/pills")
                .and_then(serde_json::Value::as_object_mut)
            {
                for pill in pills.values_mut() {
                    if let Some(pill) = pill.as_object_mut() {
                        pill.remove("style");
                    }
                }
            }
            set_json_path_value(&mut document, "version", serde_json::json!(92))?;
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
