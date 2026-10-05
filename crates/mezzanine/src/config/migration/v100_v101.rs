//! Schema v100 to v101: split prospective machine and alien naming policies.
//!
//! Only the exact historical `nonhuman` value becomes `machine`. Missing keys,
//! invalid values/types and other policies remain authored for normal validation.
//! Stored conversation identities are not part of configuration migration.

#[cfg(test)]
mod tests;

use super::ops::{
    parse_json_compatible_config, set_json_path_value, set_toml_path_item, toml_string_at,
};
use super::{ConfigFormat, MezError, Result};

/// Converts exact legacy policy in ordinary, dotted and inline documents.
pub(super) fn migrate_v100_to_v101(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            if let Some(agents) = document
                .as_table_mut()
                .get_mut("agents")
                .and_then(toml_edit::Item::as_value_mut)
                .and_then(toml_edit::Value::as_inline_table_mut)
            {
                if agents.get("name_mode").and_then(toml_edit::Value::as_str) == Some("nonhuman") {
                    agents.insert("name_mode", toml_edit::Value::from("machine"));
                }
            } else if toml_string_at(document.as_table(), "agents.name_mode").as_deref()
                == Some("nonhuman")
            {
                set_toml_path_item(
                    &mut document,
                    "agents.name_mode",
                    toml_edit::value("machine"),
                )?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(101))?;
            Ok(document.to_string())
        }
        ConfigFormat::Json | ConfigFormat::Yaml => {
            let mut document = parse_json_compatible_config(format, text)?;
            if document
                .pointer("/agents/name_mode")
                .and_then(serde_json::Value::as_str)
                == Some("nonhuman")
            {
                set_json_path_value(
                    &mut document,
                    "agents.name_mode",
                    serde_json::json!("machine"),
                )?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(101))?;
            match format {
                ConfigFormat::Json => serde_json::to_string_pretty(&document)
                    .map(|text| format!("{text}\n"))
                    .map_err(|error| MezError::config(format!("failed to render config: {error}"))),
                ConfigFormat::Yaml => serde_norway::to_string(&document)
                    .map_err(|error| MezError::config(format!("failed to render config: {error}"))),
                ConfigFormat::Toml => unreachable!("handled above"),
            }
        }
    }
}
