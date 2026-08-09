//! Configuration schema v56 to v57 migration.
//!
//! Schema v57 introduces named key-assignment presets. The migration compares
//! typed legacy key chords rather than source spelling, selects a built-in when
//! the effective map matches, and otherwise preserves the map as `migrated`.

use mez_mux::input::{KeyBindings, KeyChord};
use mez_mux::key_preset::builtin_key_preset_bindings;

use super::ops::{parse_json_compatible_config, set_json_path_value, set_toml_path_item};
use super::{ConfigFormat, MezError, Result};

/// Adds the active key preset and preserves legacy effective bindings.
pub(super) fn migrate_v56_to_v57(format: ConfigFormat, text: &str) -> Result<String> {
    match format {
        ConfigFormat::Toml => {
            let mut document = text
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let value = toml::from_str::<toml::Value>(text)
                .map_err(|error| MezError::config(format!("invalid TOML config: {error}")))?;
            let root = serde_json::to_value(value).map_err(|error| {
                MezError::config(format!("failed to convert TOML config: {error}"))
            })?;
            let preset = classify_legacy_keys(&root)?;
            set_toml_path_item(&mut document, "key_preset.active", toml_edit::value(preset))?;
            if preset == "migrated"
                && let Some(keys) = document.as_table().get("keys").cloned()
            {
                set_toml_path_item(&mut document, "key_presets.migrated", keys)?;
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(57))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            let preset = classify_legacy_keys(&document)?;
            set_json_path_value(
                &mut document,
                "key_preset.active",
                serde_json::Value::String(preset.to_string()),
            )?;
            if preset == "migrated"
                && let Some(keys) = document.get("keys").cloned()
            {
                set_json_path_value(&mut document, "key_presets.migrated", keys)?;
            }
            set_json_path_value(&mut document, "version", serde_json::json!(57))?;
            render_json_compatible(format, &document)
        }
    }
}

fn classify_legacy_keys(root: &serde_json::Value) -> Result<&'static str> {
    let bindings = legacy_bindings(root)?;
    let command_bindings_empty = root
        .get("keys")
        .and_then(serde_json::Value::as_object)
        .and_then(|keys| keys.get("command_bindings"))
        .and_then(serde_json::Value::as_object)
        .is_none_or(serde_json::Map::is_empty);
    if command_bindings_empty && Some(&bindings) == builtin_key_preset_bindings("default").as_ref()
    {
        return Ok("default");
    }
    if command_bindings_empty && Some(&bindings) == builtin_key_preset_bindings("simple").as_ref() {
        return Ok("simple");
    }
    Ok("migrated")
}

fn legacy_bindings(root: &serde_json::Value) -> Result<KeyBindings> {
    let defaults = KeyBindings::default();
    let Some(keys) = root.get("keys").and_then(serde_json::Value::as_object) else {
        return Ok(defaults);
    };
    Ok(KeyBindings {
        escape: required_chord(keys, "escape", defaults.escape)?,
        split_vertical: optional_chord(keys, "split_vertical", defaults.split_vertical)?,
        split_horizontal: optional_chord(keys, "split_horizontal", defaults.split_horizontal)?,
        new_window: optional_chord(keys, "new_window", defaults.new_window)?,
        new_group: optional_chord(keys, "new_group", defaults.new_group)?,
        agent_shell: optional_chord(keys, "agent_shell", defaults.agent_shell)?,
        focus_up: optional_chord(keys, "focus_up", defaults.focus_up)?,
        focus_down: optional_chord(keys, "focus_down", defaults.focus_down)?,
        focus_left: optional_chord(keys, "focus_left", defaults.focus_left)?,
        focus_right: optional_chord(keys, "focus_right", defaults.focus_right)?,
        focus_previous_window: optional_chord(
            keys,
            "focus_previous_window",
            defaults.focus_previous_window,
        )?,
        focus_next_window: optional_chord(keys, "focus_next_window", defaults.focus_next_window)?,
        focus_previous_group: optional_chord(
            keys,
            "focus_previous_group",
            defaults.focus_previous_group,
        )?,
        focus_next_group: optional_chord(keys, "focus_next_group", defaults.focus_next_group)?,
    })
}

fn required_chord(
    keys: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    default: KeyChord,
) -> Result<KeyChord> {
    let Some(value) = keys.get(field) else {
        return Ok(default);
    };
    let notation = value
        .as_str()
        .ok_or_else(|| MezError::config(format!("keys.{field} must be a string")))?;
    KeyChord::parse(notation)
        .map_err(|error| MezError::config(format!("keys.{field} is invalid: {error}")))
}

fn optional_chord(
    keys: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    default: Option<KeyChord>,
) -> Result<Option<KeyChord>> {
    let Some(value) = keys.get(field) else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(None);
    }
    let notation = value
        .as_str()
        .ok_or_else(|| MezError::config(format!("keys.{field} must be a string or null")))?;
    KeyChord::parse(notation)
        .map(Some)
        .map_err(|error| MezError::config(format!("keys.{field} is invalid: {error}")))
}

fn render_json_compatible(format: ConfigFormat, document: &serde_json::Value) -> Result<String> {
    match format {
        ConfigFormat::Json => serde_json::to_string_pretty(document)
            .map(|mut rendered| {
                rendered.push('\n');
                rendered
            })
            .map_err(|error| MezError::config(format!("failed to render JSON config: {error}"))),
        ConfigFormat::Yaml => serde_norway::to_string(document)
            .map_err(|error| MezError::config(format!("failed to render YAML config: {error}"))),
        ConfigFormat::Toml => unreachable!("TOML migration is handled separately"),
    }
}
