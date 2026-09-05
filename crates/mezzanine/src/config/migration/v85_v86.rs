//! Configuration schema v85 to v86 migration.
//!
//! Schema v86 makes pane status rails explicit. Existing configurations retain
//! the legacy implicit progress, agent-status, and scrollback behavior, including
//! suppression of status fields already rendered by a custom pane-title template.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

const DEFAULT_PANE_TITLE_TEMPLATE: &str = "#{pane.index} #{pane.title}";
const LEGACY_RIGHT_STATUS_FIELDS: &[&str] = &[
    "agent.model",
    "agent.reasoning",
    "agent.thinking",
    "agent.planning",
    "agent.routing",
    "agent.latency",
    "policy.mode",
    "agent.context_usage",
    "agent.status",
    "history.position",
];

/// Materializes explicit pane-status rails and advances the schema version.
pub(super) fn migrate_v85_to_v86(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let parsed = toml::from_str::<toml::Value>(text)
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let pane = parsed
                .get("frames")
                .and_then(|frames| frames.get("pane"))
                .and_then(toml::Value::as_table);
            let title = legacy_effective_title_template(
                pane.and_then(|pane| pane.get("template"))
                    .and_then(toml::Value::as_str),
                pane.and_then(|pane| pane.get("visible_fields"))
                    .and_then(toml::Value::as_array)
                    .and_then(|fields| {
                        fields
                            .iter()
                            .map(toml::Value::as_str)
                            .collect::<Option<Vec<_>>>()
                    }),
                pane.is_some(),
            );
            if pane.is_none_or(|pane| !pane.contains_key("left_status")) {
                set_toml_path_item(
                    &mut document,
                    "frames.pane.left_status",
                    toml_edit::value(legacy_left_status(&title)),
                )?;
            }
            if pane.is_none_or(|pane| !pane.contains_key("right_status")) {
                set_toml_path_item(
                    &mut document,
                    "frames.pane.right_status",
                    toml_edit::value(legacy_right_status(&title)),
                )?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(86))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            let pane = document
                .get("frames")
                .and_then(|frames| frames.get("pane"))
                .and_then(serde_json::Value::as_object);
            let title = legacy_effective_title_template(
                pane.and_then(|pane| pane.get("template"))
                    .and_then(serde_json::Value::as_str),
                pane.and_then(|pane| pane.get("visible_fields"))
                    .and_then(serde_json::Value::as_array)
                    .and_then(|fields| {
                        fields
                            .iter()
                            .map(serde_json::Value::as_str)
                            .collect::<Option<Vec<_>>>()
                    }),
                pane.is_some(),
            );
            let left_status_missing = pane.is_none_or(|pane| !pane.contains_key("left_status"));
            let right_status_missing = pane.is_none_or(|pane| !pane.contains_key("right_status"));
            if left_status_missing {
                set_json_path_value(
                    &mut document,
                    "frames.pane.left_status",
                    serde_json::json!(legacy_left_status(&title)),
                )?;
            }
            if right_status_missing {
                set_json_path_value(
                    &mut document,
                    "frames.pane.right_status",
                    serde_json::json!(legacy_right_status(&title)),
                )?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(86))?;
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

fn legacy_effective_title_template(
    configured: Option<&str>,
    visible_fields: Option<Vec<&str>>,
    pane_table_exists: bool,
) -> String {
    match configured {
        Some(template) if !template.is_empty() => template.to_string(),
        None if !pane_table_exists => DEFAULT_PANE_TITLE_TEMPLATE.to_string(),
        _ => visible_fields
            .unwrap_or_else(|| crate::host::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS.to_vec())
            .into_iter()
            .map(|field| format!("#{{{field}}}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn legacy_left_status(title: &str) -> &'static str {
    if title == DEFAULT_PANE_TITLE_TEMPLATE {
        "#{pane.progress}"
    } else {
        ""
    }
}

fn legacy_right_status(title: &str) -> String {
    LEGACY_RIGHT_STATUS_FIELDS
        .iter()
        .filter(|field| !title.contains(&format!("#{{{field}}}")))
        .map(|field| format!("#{{{field}}}"))
        .collect::<Vec<_>>()
        .join(" ")
}
