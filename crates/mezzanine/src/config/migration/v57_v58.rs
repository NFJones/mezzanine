//! Configuration schema v57 to v58 migration.
//!
//! Schema v58 introduces named key-assignment presets. Schema v57 was assigned
//! independently to the pane TERM change and to key presets on the histories
//! merged here, so this migration is deliberately tolerant of either v57
//! shape. It preserves an existing preset selection and idempotently applies
//! the pane TERM default before advancing the reconciled document.

use mez_mux::input::{KeyBindings, KeyChord};
use mez_mux::key_preset::builtin_key_preset_bindings;

use super::ops::{
    parse_json_compatible_config, set_json_default_string_if_absent_or_old_default,
    set_json_path_value, set_toml_default_string_if_absent_or_old_default, set_toml_path_item,
};
use super::{ConfigFormat, MezError, Result};

/// Adds a key preset when absent and reconciles either historical v57 shape.
pub(super) fn migrate_v57_to_v58(format: ConfigFormat, text: &str) -> Result<String> {
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
            set_toml_default_string_if_absent_or_old_default(
                &mut document,
                "terminal.term",
                "screen-256color",
                "xterm-256color",
            )?;
            if root.pointer("/key_preset/active").is_none() {
                let preset = classify_legacy_keys(&root)?;
                set_toml_path_item(&mut document, "key_preset.active", toml_edit::value(preset))?;
                if preset == "migrated"
                    && let Some(keys) = document.as_table().get("keys").cloned()
                {
                    set_toml_path_item(&mut document, "key_presets.migrated", keys)?;
                }
            }
            set_toml_path_item(&mut document, "version", toml_edit::value(58))?;
            Ok(document.to_string())
        }
        ConfigFormat::Yaml | ConfigFormat::Json => {
            let mut document = parse_json_compatible_config(format, text)?;
            set_json_default_string_if_absent_or_old_default(
                &mut document,
                "terminal.term",
                "screen-256color",
                "xterm-256color",
            )?;
            if document.pointer("/key_preset/active").is_none() {
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
            }
            set_json_path_value(&mut document, "version", serde_json::json!(58))?;
            render_json_compatible(format, &document)
        }
    }
}

/// Classifies legacy effective bindings against the built-in preset catalog.
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

/// Materializes the legacy effective key map for typed preset comparison.
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

/// Parses one required legacy binding, falling back to its former default.
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

/// Parses one optional legacy binding, including an explicit disabled value.
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

/// Renders a migrated JSON or YAML document with stable formatting.
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
