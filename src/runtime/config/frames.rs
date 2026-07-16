//! Runtime frame and keybinding option readers.
//!
//! This module owns frame decoration and keybinding materialization from the
//! effective runtime configuration value. Keeping these readers together
//! separates terminal layout and input shortcut parsing from agent, provider,
//! permission, and hook config domains.

use mez_mux::command::parse_command_sequence;
use mez_mux::input::{KeyBindings, KeyChord};
use mez_mux::presentation::{TerminalFramePosition, TerminalFrameStyle};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::command::key_chord_notation;
use crate::config::EffectiveConfig;
use crate::error::{MezError, Result};
use crate::runtime::service_state::RuntimeCommandBinding;

use super::{runtime_json_object, runtime_json_string, runtime_json_string_array};

/// Runs the runtime pane frames enabled from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_pane_frames_enabled_from_config(root: &Value) -> Result<bool> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(true);
    };
    let Some(pane) = frames.get("pane").and_then(Value::as_object) else {
        return Ok(true);
    };
    let Some(value) = pane.get("enabled") else {
        return Ok(true);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("frames.pane.enabled must be a boolean"))
}

/// Runs the runtime window frames enabled from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_window_frames_enabled_from_config(root: &Value) -> Result<bool> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(true);
    };
    let Some(window) = frames.get("window").and_then(Value::as_object) else {
        return Ok(true);
    };
    let Some(value) = window.get("enabled") else {
        return Ok(true);
    };
    value
        .as_bool()
        .ok_or_else(|| MezError::config("frames.window.enabled must be a boolean"))
}

/// Runs the runtime window frame template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_window_frame_template_from_config(
    root: &Value,
) -> Result<String> {
    runtime_frame_template_from_config(
        root,
        "window",
        crate::terminal::DEFAULT_WINDOW_FRAME_TEMPLATE,
        crate::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime window frame right status template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_window_frame_right_status_template_from_config(
    root: &Value,
) -> Result<String> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(crate::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    let Some(window) = frames.get("window").and_then(Value::as_object) else {
        return Ok(crate::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    let Some(value) = window.get("right_status") else {
        return Ok(crate::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| MezError::config("frames.window.right_status must be a string"))
}

/// Runs the runtime pane frame template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_pane_frame_template_from_config(root: &Value) -> Result<String> {
    runtime_frame_template_from_config(
        root,
        "pane",
        crate::terminal::DEFAULT_PANE_FRAME_TEMPLATE,
        crate::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime window frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_window_frame_position_from_config(
    root: &Value,
) -> Result<TerminalFramePosition> {
    runtime_frame_position_from_config(root, "window", TerminalFramePosition::Bottom)
}

/// Runs the runtime pane frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_pane_frame_position_from_config(
    root: &Value,
) -> Result<TerminalFramePosition> {
    runtime_frame_position_from_config(root, "pane", TerminalFramePosition::Top)
}

/// Runs the runtime window frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_window_frame_style_from_config(
    root: &Value,
) -> Result<TerminalFrameStyle> {
    runtime_frame_style_from_config(root, "window")
}

/// Runs the runtime pane frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_pane_frame_style_from_config(
    root: &Value,
) -> Result<TerminalFrameStyle> {
    runtime_frame_style_from_config(root, "pane")
}

/// Runs the runtime window frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_window_frame_visible_fields_from_config(
    root: &Value,
) -> Result<Vec<String>> {
    runtime_frame_visible_fields_from_config(
        root,
        "window",
        crate::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime pane frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_pane_frame_visible_fields_from_config(
    root: &Value,
) -> Result<Vec<String>> {
    runtime_frame_visible_fields_from_config(
        root,
        "pane",
        crate::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime frame template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_template_from_config(
    root: &Value,
    target: &str,
    default_template: &str,
    default_visible_fields: &[&str],
) -> Result<String> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(default_template.to_string());
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(default_template.to_string());
    };
    if let Some(value) = frame.get("template") {
        let Some(template) = value.as_str() else {
            return Err(MezError::config(format!(
                "frames.{target}.template must be a string"
            )));
        };
        if !template.is_empty() {
            return Ok(template.to_string());
        }
    }
    let visible_fields =
        runtime_frame_visible_fields_from_config(root, target, default_visible_fields)?;
    Ok(frame_template_from_visible_fields(&visible_fields))
}

/// Runs the runtime frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_position_from_config(
    root: &Value,
    target: &str,
    default: TerminalFramePosition,
) -> Result<TerminalFramePosition> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(default);
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(default);
    };
    let Some(value) = frame.get("position") else {
        return Ok(default);
    };
    let Some(position) = runtime_json_string(Some(value)) else {
        return Err(MezError::config(format!(
            "frames.{target}.position must be a string"
        )));
    };
    match position {
        "top" | "border" => Ok(TerminalFramePosition::Top),
        "bottom" => Ok(TerminalFramePosition::Bottom),
        _ => Err(MezError::config(format!(
            "frames.{target}.position must be top, bottom, or border"
        ))),
    }
}

/// Runs the runtime frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_style_from_config(root: &Value, target: &str) -> Result<TerminalFrameStyle> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(TerminalFrameStyle::Default);
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(TerminalFrameStyle::Default);
    };
    let Some(value) = frame.get("style") else {
        return Ok(TerminalFrameStyle::Default);
    };
    let Some(style) = runtime_json_string(Some(value)) else {
        return Err(MezError::config(format!(
            "frames.{target}.style must be a string"
        )));
    };
    match style {
        "default" => Ok(TerminalFrameStyle::Default),
        "bold" => Ok(TerminalFrameStyle::Bold),
        "underline" => Ok(TerminalFrameStyle::Underline),
        "inverse" | "reverse" => Ok(TerminalFrameStyle::Inverse),
        _ => Err(MezError::config(format!(
            "frames.{target}.style must be default, bold, underline, or inverse"
        ))),
    }
}

/// Runs the runtime frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_frame_visible_fields_from_config(
    root: &Value,
    target: &str,
    default_visible_fields: &[&str],
) -> Result<Vec<String>> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(default_visible_fields
            .iter()
            .map(|field| (*field).to_string())
            .collect());
    };
    let Some(frame) = frames.get(target).and_then(Value::as_object) else {
        return Ok(default_visible_fields
            .iter()
            .map(|field| (*field).to_string())
            .collect());
    };
    let fields = runtime_json_string_array(frame.get("visible_fields"))?.unwrap_or_else(|| {
        default_visible_fields
            .iter()
            .map(|field| (*field).to_string())
            .collect()
    });
    Ok(fields)
}

/// Runs the frame template from visible fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn frame_template_from_visible_fields(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| format!("#{{{field}}}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs the runtime key bindings from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_key_bindings_from_config(root: &Value) -> Result<KeyBindings> {
    let Some(keys) = runtime_json_object(root, "keys") else {
        return Ok(KeyBindings::default());
    };
    let defaults = KeyBindings::default();
    Ok(KeyBindings {
        escape: runtime_key_binding_value(keys, "escape", defaults.escape)?,
        split_vertical: runtime_optional_key_binding_value(
            keys,
            "split_vertical",
            defaults.split_vertical,
        )?,
        split_horizontal: runtime_optional_key_binding_value(
            keys,
            "split_horizontal",
            defaults.split_horizontal,
        )?,
        new_window: runtime_optional_key_binding_value(keys, "new_window", defaults.new_window)?,
        new_group: runtime_optional_key_binding_value(keys, "new_group", defaults.new_group)?,
        agent_shell: runtime_optional_key_binding_value(keys, "agent_shell", defaults.agent_shell)?,
        focus_up: runtime_optional_key_binding_value(keys, "focus_up", defaults.focus_up)?,
        focus_down: runtime_optional_key_binding_value(keys, "focus_down", defaults.focus_down)?,
        focus_left: runtime_optional_key_binding_value(keys, "focus_left", defaults.focus_left)?,
        focus_right: runtime_optional_key_binding_value(keys, "focus_right", defaults.focus_right)?,
        focus_previous_window: runtime_optional_key_binding_value(
            keys,
            "focus_previous_window",
            defaults.focus_previous_window,
        )?,
        focus_next_window: runtime_optional_key_binding_value(
            keys,
            "focus_next_window",
            defaults.focus_next_window,
        )?,
        focus_previous_group: runtime_optional_key_binding_value(
            keys,
            "focus_previous_group",
            defaults.focus_previous_group,
        )?,
        focus_next_group: runtime_optional_key_binding_value(
            keys,
            "focus_next_group",
            defaults.focus_next_group,
        )?,
    })
}

/// Runs the runtime key binding value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_key_binding_value(
    keys: &serde_json::Map<String, Value>,
    field: &str,
    default: KeyChord,
) -> Result<KeyChord> {
    let Some(value) = keys.get(field) else {
        return Ok(default);
    };
    let Some(notation) = value.as_str() else {
        return Err(MezError::config(format!("keys.{field} must be a string")));
    };
    KeyChord::parse(notation)
        .map_err(|error| MezError::config(format!("keys.{field} is invalid: {error}")))
}

/// Reads an optional direct key binding from effective configuration.
///
/// Missing fields keep the generated default. A string configures the direct
/// binding, while `null` disables it explicitly.
///
/// # Parameters
/// - `keys`: The effective `[keys]` object.
/// - `field`: The direct binding field name.
/// - `default`: The generated default binding state.
pub(in crate::runtime) fn runtime_optional_key_binding_value(
    keys: &serde_json::Map<String, Value>,
    field: &str,
    default: Option<KeyChord>,
) -> Result<Option<KeyChord>> {
    let Some(value) = keys.get(field) else {
        return Ok(default);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(notation) = value.as_str() else {
        return Err(MezError::config(format!(
            "keys.{field} must be a string or null"
        )));
    };
    KeyChord::parse(notation)
        .map(Some)
        .map_err(|error| MezError::config(format!("keys.{field} is invalid: {error}")))
}

/// Runs the runtime command bindings from effective operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_command_bindings_from_effective(
    effective: &EffectiveConfig,
) -> Result<BTreeMap<KeyChord, RuntimeCommandBinding>> {
    let mut bindings = BTreeMap::new();
    for (path, value) in effective.values() {
        let Some(config_key) = path.strip_prefix("keys.command_bindings.") else {
            continue;
        };
        let (chord, notation) = runtime_chord_from_binding_config_key(config_key)?;
        parse_command_sequence(&value.value).map_err(|error| {
            MezError::config(format!(
                "keys.command_bindings.{config_key} command is invalid: {error}"
            ))
        })?;
        bindings.insert(
            chord,
            RuntimeCommandBinding {
                notation,
                command: value.value.clone(),
                source_layer: value.source_layer.clone(),
            },
        );
    }
    Ok(bindings)
}

/// Runs the runtime chord from binding config key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_chord_from_binding_config_key(
    config_key: &str,
) -> Result<(KeyChord, String)> {
    let notation = if let Some(encoded) = config_key.strip_prefix("key_") {
        runtime_decode_binding_config_key(encoded)?
    } else {
        config_key.to_string()
    };
    let chord = KeyChord::parse(&notation).map_err(|error| {
        MezError::config(format!(
            "keys.command_bindings.{config_key} is not a valid key binding: {error}"
        ))
    })?;
    Ok((chord, key_chord_notation(chord)))
}

/// Runs the runtime decode binding config key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_decode_binding_config_key(encoded: &str) -> Result<String> {
    if encoded.is_empty() {
        return Err(MezError::config("encoded key binding must not be empty"));
    }
    let mut bytes = Vec::new();
    for segment in encoded.split('_') {
        if segment.len() != 2 {
            return Err(MezError::config("encoded key binding segment is invalid"));
        }
        let byte = u8::from_str_radix(segment, 16)
            .map_err(|_| MezError::config("encoded key binding segment is not hexadecimal"))?;
        bytes.push(byte);
    }
    String::from_utf8(bytes).map_err(|_| MezError::config("encoded key binding is not valid UTF-8"))
}
