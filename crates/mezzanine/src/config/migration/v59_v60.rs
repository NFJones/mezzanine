//! Configuration schema v59 to v60 migration.
//!
//! Schema v60 adds the approval-attention theme color used by pane, window,
//! and group pills while an agent action is waiting for approval. Existing
//! configurations receive a danger-backed default distinct from completion
//! attention.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the approval-attention color pair and advances the schema version.
pub(super) fn migrate_v59_to_v60(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            set_toml_path_item(
                &mut document,
                "theme.colors.agent_approval_attention_fg",
                toml_edit::value("danger_text"),
            )?;
            set_toml_path_item(
                &mut document,
                "theme.colors.agent_approval_attention_bg",
                toml_edit::value("danger"),
            )?;
            set_toml_path_item(&mut document, "version", toml_edit::value(60))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_path_value(
                &mut document,
                "theme.colors.agent_approval_attention_fg",
                serde_json::json!("danger_text"),
            )?;
            set_json_path_value(
                &mut document,
                "theme.colors.agent_approval_attention_bg",
                serde_json::json!("danger"),
            )?;
            set_json_path_value(&mut document, "version", serde_json::json!(60))?;
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
