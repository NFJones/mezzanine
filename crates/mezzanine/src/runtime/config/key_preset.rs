//! Runtime key-assignment preset resolution.
//!
//! This module converts the active built-in or configured preset into typed
//! mux bindings. The materialized `[keys]` table remains a separate override
//! surface so low-level key edits continue to take precedence.

use mez_mux::input::{KeyBindings, KeyChord};
use mez_mux::key_preset::{
    DEFAULT_KEY_PRESET_NAME, KeyPresetDefinition, builtin_key_preset_definition,
};
use serde_json::Value;

use crate::error::{MezError, Result};

/// Returns the active key-preset name from structured effective config.
pub(super) fn runtime_active_key_preset_name(root: &Value) -> Result<&str> {
    let active = root
        .get("key_preset")
        .and_then(Value::as_object)
        .and_then(|preset| preset.get("active"))
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_KEY_PRESET_NAME);
    if active.trim().is_empty() {
        return Err(MezError::config(
            "key_preset.active must be a non-empty preset identifier",
        ));
    }
    Ok(active)
}

/// Resolves the active preset into typed mux bindings and command definitions.
pub(crate) fn runtime_active_key_preset(
    root: &Value,
) -> Result<(String, KeyBindings, KeyPresetDefinition)> {
    let active = runtime_active_key_preset_name(root)?;
    let definition = if let Some(definition) = builtin_key_preset_definition(active) {
        definition
    } else {
        let value = root
            .get("key_presets")
            .and_then(Value::as_object)
            .and_then(|presets| presets.get(active))
            .ok_or_else(|| {
                MezError::config(format!(
                    "key_preset.active `{active}` does not name a built-in or configured preset"
                ))
            })?;
        runtime_key_preset_definition_from_value(value, &format!("key_presets.{active}"))?
    };
    let bindings = definition.materialize(KeyBindings::default());
    Ok((active.to_string(), bindings, definition))
}

/// Parses one configured partial key-preset definition.
pub(crate) fn runtime_key_preset_definition_from_value(
    value: &Value,
    path: &str,
) -> Result<KeyPresetDefinition> {
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    Ok(KeyPresetDefinition {
        escape: preset_required_chord(object.get("escape"), &format!("{path}.escape"))?,
        split_vertical: preset_optional_chord(
            object.get("split_vertical"),
            &format!("{path}.split_vertical"),
        )?,
        split_horizontal: preset_optional_chord(
            object.get("split_horizontal"),
            &format!("{path}.split_horizontal"),
        )?,
        new_window: preset_optional_chord(object.get("new_window"), &format!("{path}.new_window"))?,
        new_group: preset_optional_chord(object.get("new_group"), &format!("{path}.new_group"))?,
        agent_shell: preset_optional_chord(
            object.get("agent_shell"),
            &format!("{path}.agent_shell"),
        )?,
        focus_up: preset_optional_chord(object.get("focus_up"), &format!("{path}.focus_up"))?,
        focus_down: preset_optional_chord(object.get("focus_down"), &format!("{path}.focus_down"))?,
        focus_left: preset_optional_chord(object.get("focus_left"), &format!("{path}.focus_left"))?,
        focus_right: preset_optional_chord(
            object.get("focus_right"),
            &format!("{path}.focus_right"),
        )?,
        focus_previous_window: preset_optional_chord(
            object.get("focus_previous_window"),
            &format!("{path}.focus_previous_window"),
        )?,
        focus_next_window: preset_optional_chord(
            object.get("focus_next_window"),
            &format!("{path}.focus_next_window"),
        )?,
        focus_previous_group: preset_optional_chord(
            object.get("focus_previous_group"),
            &format!("{path}.focus_previous_group"),
        )?,
        focus_next_group: preset_optional_chord(
            object.get("focus_next_group"),
            &format!("{path}.focus_next_group"),
        )?,
        command_bindings: preset_command_bindings(
            object.get("command_bindings"),
            &format!("{path}.command_bindings"),
        )?,
    })
}

fn preset_required_chord(value: Option<&Value>, path: &str) -> Result<Option<KeyChord>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let notation = value
        .as_str()
        .ok_or_else(|| MezError::config(format!("{path} must be a string")))?;
    KeyChord::parse(notation)
        .map(Some)
        .map_err(|error| MezError::config(format!("{path} is invalid: {error}")))
}

fn preset_optional_chord(value: Option<&Value>, path: &str) -> Result<Option<Option<KeyChord>>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let notation = value
        .as_str()
        .ok_or_else(|| MezError::config(format!("{path} must be a string or null")))?;
    KeyChord::parse(notation)
        .map(|chord| Some(Some(chord)))
        .map_err(|error| MezError::config(format!("{path} is invalid: {error}")))
}

fn preset_command_bindings(
    value: Option<&Value>,
    path: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(std::collections::BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| MezError::config(format!("{path} must be a table")))?;
    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|command| (key.clone(), command.to_string()))
                .ok_or_else(|| MezError::config(format!("{path}.{key} must be a string")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_mux::input::KeyCode;

    /// Verifies a configured preset inherits omitted fields from the default
    /// preset while preserving explicit null direct bindings.
    #[test]
    fn configured_preset_resolves_inheritance_and_null() {
        let root = serde_json::json!({
            "key_preset": {"active": "custom"},
            "key_presets": {"custom": {
                "new_window": "A-n",
                "new_group": null,
                "command_bindings": {"x": "new-window"}
            }}
        });
        let (name, bindings, definition) = runtime_active_key_preset(&root).unwrap();
        assert_eq!(name, "custom");
        assert_eq!(bindings.escape, KeyChord::ctrl(KeyCode::Char('a')));
        assert_eq!(bindings.new_window, Some(KeyChord::alt(KeyCode::Char('n'))));
        assert_eq!(bindings.new_group, None);
        assert_eq!(
            definition.command_bindings.get("x"),
            Some(&"new-window".to_string())
        );
    }

    /// Verifies an unknown active preset is rejected before runtime input state
    /// can be partially replaced.
    #[test]
    fn unknown_active_preset_is_rejected() {
        let error = runtime_active_key_preset(&serde_json::json!({
            "key_preset": {"active": "missing"}
        }))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not name a built-in or configured preset")
        );
    }
}
