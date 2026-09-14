//! Configuration schema v95 to v96 migration.
//!
//! Schema v96 promotes sandbox environment forwarding to one shared
//! `permissions.env_whitelist` setting so native policy-only workloads and
//! both sandbox backends use the same pane-derived names.

use super::ops::{
    json_value_at, parse_json_compatible_config, remove_json_path, remove_toml_path,
    set_json_path_value, set_toml_path_item, toml_item_at,
};
use super::{ConfigFormat, MezError, Result};

/// Promotes backend-scoped whitelist values and advances one document to v96.
pub(super) fn migrate_v95_to_v96(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            if toml_item_at(document.as_table(), "permissions.env_whitelist").is_none() {
                let legacy =
                    toml_item_at(document.as_table(), "permissions.bubblewrap.env_whitelist")
                        .or_else(|| {
                            toml_item_at(document.as_table(), "permissions.seatbelt.env_whitelist")
                        })
                        .cloned();
                if let Some(legacy) = legacy {
                    set_toml_path_item(&mut document, "permissions.env_whitelist", legacy)?;
                }
            }
            remove_toml_path(&mut document, "permissions.bubblewrap.env_whitelist")?;
            remove_toml_path(&mut document, "permissions.seatbelt.env_whitelist")?;
            set_toml_path_item(&mut document, "version", toml_edit::value(96))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            if json_value_at(&document, "permissions.env_whitelist").is_none() {
                let legacy = json_value_at(&document, "permissions.bubblewrap.env_whitelist")
                    .or_else(|| json_value_at(&document, "permissions.seatbelt.env_whitelist"))
                    .cloned();
                if let Some(legacy) = legacy {
                    set_json_path_value(&mut document, "permissions.env_whitelist", legacy)?;
                }
            }
            remove_json_path(&mut document, "permissions.bubblewrap.env_whitelist");
            remove_json_path(&mut document, "permissions.seatbelt.env_whitelist");
            set_json_path_value(&mut document, "version", serde_json::json!(96))?;
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
