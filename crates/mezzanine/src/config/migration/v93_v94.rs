//! Configuration schema v93 to v94 migration.
//!
//! Schema v94 adds the provider-visible `wait` action. Only the exact former
//! generated default allowlist receives the new action automatically; custom
//! allowlists remain unchanged so migration never broadens explicit policy.

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Exact generated action allowlist used by schema v93.
const V93_DEFAULT_ACTIONS: [&str; 18] = [
    "say",
    "shell_command",
    "apply_patch",
    "web_search",
    "fetch_url",
    "send_message",
    "spawn_agent",
    "config_change",
    "mcp_server_search",
    "mcp_server_get",
    "mcp_call",
    "memory_search",
    "memory_store",
    "list_agents",
    "issue_add",
    "issue_update",
    "issue_query",
    "issue_delete",
];

/// Inserts `wait` after `send_message` when the allowlist is the v93 default.
fn migrated_actions(values: &[String]) -> Option<Vec<String>> {
    if values.len() != V93_DEFAULT_ACTIONS.len()
        || !values.iter().map(String::as_str).eq(V93_DEFAULT_ACTIONS)
    {
        return None;
    }
    let mut migrated = values.to_vec();
    migrated.insert(6, "wait".to_string());
    Some(migrated)
}

/// Adds `wait` to the former generated default action catalog and advances the
/// document to schema v94.
pub(super) fn migrate_v93_to_v94(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let configured = document
                .get("agents")
                .and_then(toml_edit::Item::as_table)
                .and_then(|agents| agents.get("enabled_actions"))
                .and_then(toml_edit::Item::as_value)
                .and_then(toml_edit::Value::as_array)
                .and_then(|array| {
                    array
                        .iter()
                        .map(|value| value.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                });
            if let Some(actions) = configured.as_deref().and_then(migrated_actions) {
                let mut array = toml_edit::Array::new();
                for action in actions {
                    array.push(action);
                }
                set_toml_path_item(
                    &mut document,
                    "agents.enabled_actions",
                    toml_edit::value(array),
                )?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(94))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            let configured = document
                .pointer("/agents/enabled_actions")
                .and_then(serde_json::Value::as_array)
                .and_then(|array| {
                    array
                        .iter()
                        .map(|value| value.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                });
            if let Some(actions) = configured.as_deref().and_then(migrated_actions) {
                set_json_path_value(
                    &mut document,
                    "agents.enabled_actions",
                    serde_json::json!(actions),
                )?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(94))?;
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
