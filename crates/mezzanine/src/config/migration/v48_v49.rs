//! Configuration schema v48 to v49 migration.
//!
//! Schema v49 renames the Bubblewrap pane-group mapping allowlist from
//! `supplementary_groups` to `group_whitelist` without changing its authority.

use super::ops::{
    json_value_at, normalize_json_rename, normalize_toml_rename, parse_json_compatible_config,
    set_json_path_value, set_toml_path_item, toml_item_at,
};
use super::{ConfigFormat, MezError, Result};

const OLD_PATH: &str = "permissions.bubblewrap.supplementary_groups";
const NEW_PATH: &str = "permissions.bubblewrap.group_whitelist";

/// Renames the configured pane-group mapping allowlist and advances to v49.
pub(super) fn migrate_v48_to_v49(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            if toml_item_at(document.as_table(), OLD_PATH).is_some()
                && toml_item_at(document.as_table(), NEW_PATH).is_some()
            {
                return Err(MezError::config(format!(
                    "configuration defines both {OLD_PATH} and {NEW_PATH}"
                )));
            }
            normalize_toml_rename(&mut document, OLD_PATH, NEW_PATH)?;
            set_toml_path_item(&mut document, "version", toml_edit::value(49))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            if json_value_at(&document, OLD_PATH).is_some()
                && json_value_at(&document, NEW_PATH).is_some()
            {
                return Err(MezError::config(format!(
                    "configuration defines both {OLD_PATH} and {NEW_PATH}"
                )));
            }
            normalize_json_rename(&mut document, OLD_PATH, NEW_PATH)?;
            set_json_path_value(&mut document, "version", serde_json::json!(49))?;
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
