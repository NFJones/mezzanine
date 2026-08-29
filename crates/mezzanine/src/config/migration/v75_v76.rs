//! Configuration schema v75 to v76 migration.
//!
//! Schema v76 moves pane-title pill padding from the built-in template into
//! the renderer. Only the exact historical default is rewritten so custom
//! templates retain their caller-authored whitespace and content.

use super::ops::{
    json_value_at, parse_json_compatible_config, set_json_path_value, set_toml_path_item,
    toml_item_at,
};
use super::{ConfigFormat, MezError, Result};

const OLD_DEFAULT_PANE_FRAME_TEMPLATE: &str = " #{pane.index} #{pane.title} ";
const NEW_DEFAULT_PANE_FRAME_TEMPLATE: &str = "#{pane.index} #{pane.title}";

/// Rewrites the historical default pane-frame template and advances to v76.
pub(super) fn migrate_v75_to_v76(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            if toml_item_at(document.as_table(), "frames.pane.template")
                .and_then(toml_edit::Item::as_str)
                == Some(OLD_DEFAULT_PANE_FRAME_TEMPLATE)
            {
                set_toml_path_item(
                    &mut document,
                    "frames.pane.template",
                    toml_edit::value(NEW_DEFAULT_PANE_FRAME_TEMPLATE),
                )?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(76))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            if json_value_at(&document, "frames.pane.template").and_then(serde_json::Value::as_str)
                == Some(OLD_DEFAULT_PANE_FRAME_TEMPLATE)
            {
                set_json_path_value(
                    &mut document,
                    "frames.pane.template",
                    serde_json::json!(NEW_DEFAULT_PANE_FRAME_TEMPLATE),
                )?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(76))?;
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
