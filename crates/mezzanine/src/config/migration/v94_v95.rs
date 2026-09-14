//! Configuration schema v94 to v95 migration.
//!
//! Schema v95 removes the outbound peer-transcript marker theme slots because
//! peer pane logs are emitted only at receiving endpoints.

use super::ops::{
    copy_json_default_if_absent, parse_json_compatible_config, remove_json_path,
    set_json_path_value, set_toml_path_item,
};
use super::{ConfigFormat, DEFAULT_CONFIG_TOML, MezError, Result};

/// Removes retired outbound peer marker colors and advances one document to v95.
pub(super) fn migrate_v94_to_v95(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let defaults = DEFAULT_CONFIG_TOML
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| {
                    MezError::config(format!("invalid default TOML config: {error}"))
                })?;
            remove_toml_theme_colors(&mut document)?;
            materialize_toml_subagent_name_mode(&mut document, &defaults)?;
            set_toml_path_item(&mut document, "version", toml_edit::value(95))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            let defaults = toml::from_str::<toml::Value>(DEFAULT_CONFIG_TOML).map_err(|error| {
                MezError::config(format!("invalid default TOML config: {error}"))
            })?;
            let defaults = serde_json::to_value(defaults).map_err(|error| {
                MezError::config(format!("failed to convert default config: {error}"))
            })?;
            remove_json_path(
                &mut document,
                "theme.colors.agent_transcript_peer_receiver_fg",
            );
            remove_json_path(
                &mut document,
                "theme.colors.agent_transcript_peer_receiver_bg",
            );
            remove_json_theme_colors(&mut document);
            copy_json_default_if_absent(&mut document, &defaults, "agents.subagent_name_mode")?;
            set_json_path_value(&mut document, "version", serde_json::json!(95))?;
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

/// Materializes the default mode without treating a valid inline agents table
/// as a scalar parent.
fn materialize_toml_subagent_name_mode(
    document: &mut toml_edit::DocumentMut,
    defaults: &toml_edit::DocumentMut,
) -> Result<()> {
    if let Some(agents) = document
        .as_table_mut()
        .get_mut("agents")
        .and_then(toml_edit::Item::as_value_mut)
        .and_then(toml_edit::Value::as_inline_table_mut)
    {
        if !agents.contains_key("subagent_name_mode") {
            agents.insert("subagent_name_mode", toml_edit::Value::from("nonhuman"));
        }
        return Ok(());
    }
    if document
        .as_table()
        .get("agents")
        .and_then(toml_edit::Item::as_table)
        .and_then(|agents| agents.get("subagent_name_mode"))
        .is_none()
    {
        let default = defaults
            .as_table()
            .get("agents")
            .and_then(toml_edit::Item::as_table)
            .and_then(|agents| agents.get("subagent_name_mode"))
            .cloned()
            .ok_or_else(|| MezError::config("default subagent name mode is missing"))?;
        set_toml_path_item(document, "agents.subagent_name_mode", default)?;
    }
    Ok(())
}

/// Removes retired peer-marker colors from TOML tables and inline tables.
fn remove_toml_theme_colors(document: &mut toml_edit::DocumentMut) -> Result<()> {
    if let Some(theme) = document.as_table_mut().get_mut("theme") {
        remove_toml_receiver_colors_from_item(theme);
    }
    if let Some(themes) = document.as_table_mut().get_mut("themes") {
        if let Some(themes) = themes.as_table_mut() {
            for (_, theme) in themes.iter_mut() {
                remove_toml_receiver_colors_from_item(theme);
            }
        } else if let Some(themes) = themes
            .as_value_mut()
            .and_then(toml_edit::Value::as_inline_table_mut)
        {
            for (_, theme) in themes.iter_mut() {
                remove_toml_receiver_colors_from_value(theme);
            }
        }
    }
    Ok(())
}

/// Removes retired peer-marker colors from one TOML theme item.
fn remove_toml_receiver_colors_from_item(theme: &mut toml_edit::Item) {
    if let Some(theme) = theme.as_table_mut() {
        theme.remove("agent_transcript_peer_receiver_fg");
        theme.remove("agent_transcript_peer_receiver_bg");
        if let Some(colors) = theme.get_mut("colors") {
            remove_toml_receiver_colors_from_item(colors);
        }
    } else if let Some(theme) = theme.as_value_mut() {
        remove_toml_receiver_colors_from_value(theme);
    }
}

/// Removes retired peer-marker colors from one TOML inline theme value.
fn remove_toml_receiver_colors_from_value(theme: &mut toml_edit::Value) {
    let Some(theme) = theme.as_inline_table_mut() else {
        return;
    };
    theme.remove("agent_transcript_peer_receiver_fg");
    theme.remove("agent_transcript_peer_receiver_bg");
    if let Some(colors) = theme.get_mut("colors") {
        remove_toml_receiver_colors_from_value(colors);
    }
}

/// Removes retired peer-marker colors from every named JSON-compatible theme.
fn remove_json_theme_colors(document: &mut serde_json::Value) {
    let names = document
        .get("themes")
        .and_then(serde_json::Value::as_object)
        .map(|themes| themes.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for name in names {
        remove_json_path(
            document,
            &format!("themes.{name}.colors.agent_transcript_peer_receiver_fg"),
        );
        remove_json_path(
            document,
            &format!("themes.{name}.colors.agent_transcript_peer_receiver_bg"),
        );
    }
}
