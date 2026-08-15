//! Configuration schema v62 to v63 migration.
//!
//! Schema v63 adds the pane-frame `agent.planning` field. Existing primary
//! configurations receive it immediately after `agent.thinking` without
//! duplicating an explicitly configured occurrence.

use super::ops::{
    ensure_json_agent_planning_visible_field, ensure_toml_agent_planning_visible_field,
    parse_json_compatible_config, set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, MezError, Result};

/// Adds the plan-only status field and advances the schema version.
pub(super) fn migrate_v62_to_v63(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            ensure_toml_agent_planning_visible_field(&mut document)?;
            set_toml_path_item(&mut document, "version", toml_edit::value(63))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            ensure_json_agent_planning_visible_field(&mut document)?;
            set_json_path_value(&mut document, "version", serde_json::json!(63))?;
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
