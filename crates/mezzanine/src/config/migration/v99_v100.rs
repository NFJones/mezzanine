//! Schema v99 to v100: generalize child naming to all native agent identities.
//!
//! Canonical name_mode wins if both spellings exist; authored policy is retained
//! and the obsolete key is removed without allocating any runtime identity.

use super::ops::{
    normalize_json_rename, normalize_toml_rename, set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, MezError, Result};

/// Renames the naming policy while preserving canonical-key precedence.
pub(super) fn migrate_v99_to_v100(format: ConfigFormat, text: &str) -> Result<String> {
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
                let old = agents.remove("subagent_name_mode");
                if !agents.contains_key("name_mode")
                    && let Some(value) = old
                {
                    agents.insert("name_mode", value);
                }
            } else {
                normalize_toml_rename(
                    &mut document,
                    "agents.subagent_name_mode",
                    "agents.name_mode",
                )?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(100))?;
            Ok(document.to_string())
        }
        ConfigFormat::Json | ConfigFormat::Yaml => {
            let mut document = super::ops::parse_json_compatible_config(format, text)?;
            normalize_json_rename(
                &mut document,
                "agents.subagent_name_mode",
                "agents.name_mode",
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(100))?;
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
