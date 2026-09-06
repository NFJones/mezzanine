//! Runtime frame and keybinding option readers.
//!
//! This module owns frame decoration and keybinding materialization from the
//! effective runtime configuration value. Keeping these readers together
//! separates terminal layout and input shortcut parsing from agent, provider,
//! permission, and hook config domains.

use mez_agent::parse_slash_command;
use mez_mux::command::parse_command_sequence;
use mez_mux::input::{ConfigurableKeyAction, KeyBindings, KeyChord, classify_prefix_binding};
use mez_mux::presentation::{TerminalFramePosition, TerminalFrameStyle};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::config::{ConfigLayer, ConfigScope, EffectiveConfig};
use crate::error::{MezError, Result};
use crate::host::terminal::{
    PaneStatusAction, PaneStatusCondition, PaneStatusConfig, PaneStatusField, PaneStatusFormat,
    PaneStatusOverflowPolicy, PaneStatusPillDefinition, PaneStatusProviderDefinition,
    PaneStatusProviderEmptyBehavior, PaneStatusProviderErrorBehavior, PaneStatusProviderOrigin,
    PaneStatusProviderScope, PaneStatusStyle, PaneStatusTerminalAction, PaneStatusTerminalCommand,
};
use crate::runtime::service_state::RuntimeCommandBinding;
use crate::runtime::status_pills::{
    DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS, DEFAULT_STATUS_PILL_TIMEOUT_MS,
};
use crate::ui::command::key_chord_notation;

use super::{
    runtime_active_key_preset, runtime_json_object, runtime_json_string, runtime_json_string_array,
};

/// Maximum wall-clock duration allowed for one passive pane-status provider.
const MAX_PANE_STATUS_PROVIDER_TIMEOUT_MS: u64 = 60_000;

/// Runs the runtime pane frames enabled from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_pane_frames_enabled_from_config(root: &Value) -> Result<bool> {
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
pub(crate) fn runtime_window_frames_enabled_from_config(root: &Value) -> Result<bool> {
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
pub(crate) fn runtime_window_frame_template_from_config(root: &Value) -> Result<String> {
    runtime_frame_template_from_config(
        root,
        "window",
        crate::host::terminal::DEFAULT_WINDOW_FRAME_TEMPLATE,
        crate::host::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime window frame right status template from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_window_frame_right_status_template_from_config(
    root: &Value,
) -> Result<String> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(crate::host::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    let Some(window) = frames.get("window").and_then(Value::as_object) else {
        return Ok(crate::host::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
    };
    let Some(value) = window.get("right_status") else {
        return Ok(crate::host::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string());
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
pub(crate) fn runtime_pane_frame_template_from_config(root: &Value) -> Result<String> {
    runtime_frame_template_from_config(
        root,
        "pane",
        crate::host::terminal::DEFAULT_PANE_FRAME_TEMPLATE,
        crate::host::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
    )
}

/// Parses pane-status rails and named built-in pill definitions.
///
/// The complete value is built before runtime presentation settings are
/// replaced, so an invalid definition cannot partially update a live frame.
pub(crate) fn runtime_pane_status_config_from_config(root: &Value) -> Result<PaneStatusConfig> {
    runtime_pane_status_config(root, None, &[])
}

/// Parses pane status configuration and retains exact executable-source provenance.
pub(crate) fn runtime_pane_status_config_from_effective(
    root: &Value,
    effective: &EffectiveConfig,
    layers: &[ConfigLayer],
) -> Result<PaneStatusConfig> {
    runtime_pane_status_config(root, Some(effective), layers)
}

fn runtime_pane_status_config(
    root: &Value,
    effective: Option<&EffectiveConfig>,
    layers: &[ConfigLayer],
) -> Result<PaneStatusConfig> {
    let Some(frames) = runtime_json_object(root, "frames") else {
        return Ok(PaneStatusConfig::default());
    };
    let Some(pane) = frames.get("pane").and_then(Value::as_object) else {
        return Ok(PaneStatusConfig::default());
    };
    let preset_name = pane
        .get("status_preset")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| MezError::config("frames.pane.status_preset must be a string"))
        })
        .transpose()?
        .unwrap_or("standard");
    let mut config = PaneStatusConfig::from_preset_name(preset_name).ok_or_else(|| {
        MezError::config(
            "frames.pane.status_preset must be standard, minimal, agent-focused, or full-controls",
        )
    })?;
    debug_assert_eq!(config.preset_name(), preset_name);
    if let Some(value) = pane.get("left_status") {
        config.left_status = value
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| MezError::config("frames.pane.left_status must be a string"))?;
    }
    if let Some(value) = pane.get("right_status") {
        config.right_status = value
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| MezError::config("frames.pane.right_status must be a string"))?;
    }
    if let Some(value) = pane.get("overflow") {
        config.overflow = match value.as_str() {
            Some("compact") => PaneStatusOverflowPolicy::Compact,
            Some("hide") => PaneStatusOverflowPolicy::Hide,
            Some("menu") => PaneStatusOverflowPolicy::Menu,
            _ => {
                return Err(MezError::config(
                    "frames.pane.overflow must be compact, hide, or menu",
                ));
            }
        };
    }
    if let Some(value) = pane.get("title_min_width") {
        config.title_min_width = value
            .as_u64()
            .filter(|width| (1..=4096).contains(width))
            .and_then(|width| usize::try_from(width).ok())
            .ok_or_else(|| {
                MezError::config("frames.pane.title_min_width must be an integer from 1 to 4096")
            })?;
    }
    let Some(pills_value) = pane.get("pills") else {
        return Ok(config);
    };
    let pills = pills_value
        .as_object()
        .ok_or_else(|| MezError::config("frames.pane.pills must be a table"))?;
    for (name, value) in pills {
        if name.is_empty()
            || !name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err(MezError::config(format!(
                "frames.pane.pills.{name} name must contain only ASCII letters, digits, underscores, or hyphens"
            )));
        }
        let object = value
            .as_object()
            .ok_or_else(|| MezError::config(format!("frames.pane.pills.{name} must be a table")))?;
        for key in object.keys() {
            if !matches!(
                key.as_str(),
                "field"
                    | "command"
                    | "cwd"
                    | "interval_seconds"
                    | "initial"
                    | "timeout_ms"
                    | "empty_behavior"
                    | "error_behavior"
                    | "max_output_chars"
                    | "label"
                    | "format"
                    | "compact_format"
                    | "when"
                    | "min_width"
                    | "max_width"
                    | "priority"
                    | "style"
                    | "on_click"
            ) {
                return Err(MezError::config(format!(
                    "frames.pane.pills.{name}.{key} is not a supported pane status pill setting"
                )));
            }
        }
        let preset_definition = config.pills.get(name).cloned();
        let field_name = object.get("field").and_then(Value::as_str);
        let command = optional_pane_status_string(object.get("command"), name, "command")?
            .filter(|value| !value.trim().is_empty());
        if field_name.is_some() && command.is_some() {
            return Err(MezError::config(format!(
                "frames.pane.pills.{name} must configure exactly one of field or command"
            )));
        }
        let field = if let Some(field_name) = field_name {
            PaneStatusField::parse(field_name).ok_or_else(|| {
                MezError::config(format!(
                    "frames.pane.pills.{name}.field `{field_name}` is not a supported built-in pane status field"
                ))
            })?
        } else if command.is_some() {
            PaneStatusField::Provider
        } else if let Some(definition) = preset_definition.as_ref() {
            definition.field
        } else {
            return Err(MezError::config(format!(
                "frames.pane.pills.{name} must configure exactly one of field or command"
            )));
        };
        let mut definition =
            preset_definition.unwrap_or_else(|| PaneStatusPillDefinition::builtin(field));
        if field_name.is_some() {
            definition.field = field;
            definition.provider = None;
            definition.action = field
                .builtin_action()
                .map(PaneStatusAction::Builtin)
                .unwrap_or(PaneStatusAction::None);
        } else if command.is_some() {
            definition.field = PaneStatusField::Provider;
            definition.provider = None;
            definition.action = PaneStatusAction::None;
        }
        if let Some(command) = command {
            if object.get("cwd").and_then(Value::as_str) != Some("pane") {
                return Err(MezError::config(format!(
                    "frames.pane.pills.{name}.cwd must be pane for command providers"
                )));
            }
            let interval_seconds = positive_pane_status_u64(
                object.get("interval_seconds"),
                name,
                "interval_seconds",
                30,
            )?;
            let timeout_ms = positive_pane_status_u64(
                object.get("timeout_ms"),
                name,
                "timeout_ms",
                DEFAULT_STATUS_PILL_TIMEOUT_MS,
            )?;
            if timeout_ms > MAX_PANE_STATUS_PROVIDER_TIMEOUT_MS {
                return Err(MezError::config(format!(
                    "frames.pane.pills.{name}.timeout_ms must not exceed {MAX_PANE_STATUS_PROVIDER_TIMEOUT_MS}"
                )));
            }
            let max_output_chars = positive_pane_status_u64(
                object.get("max_output_chars"),
                name,
                "max_output_chars",
                DEFAULT_STATUS_PILL_MAX_OUTPUT_CHARS as u64,
            )?;
            definition.provider = Some(PaneStatusProviderDefinition {
                command,
                origin: pane_status_provider_origin(effective, layers, name, "command"),
                interval_ms: interval_seconds.saturating_mul(1_000),
                initial: optional_pane_status_string(object.get("initial"), name, "initial")?,
                timeout_ms,
                empty_behavior: parse_pane_status_empty_behavior(
                    object.get("empty_behavior"),
                    name,
                )?,
                error_behavior: parse_pane_status_error_behavior(
                    object.get("error_behavior"),
                    name,
                )?,
                max_output_chars: usize::try_from(max_output_chars).map_err(|_| {
                    MezError::config(format!(
                        "frames.pane.pills.{name}.max_output_chars is too large"
                    ))
                })?,
            });
            definition.when.clear();
        } else if object.contains_key("cwd")
            || object.contains_key("interval_seconds")
            || object.contains_key("initial")
            || object.contains_key("timeout_ms")
            || object.contains_key("empty_behavior")
            || object.contains_key("error_behavior")
            || object.contains_key("max_output_chars")
        {
            return Err(MezError::config(format!(
                "frames.pane.pills.{name} provider settings require command"
            )));
        }
        if object.contains_key("label") {
            definition.label = optional_pane_status_string(object.get("label"), name, "label")?;
        }
        if let Some(value) = optional_pane_status_string(object.get("format"), name, "format")? {
            definition.format = parse_pane_status_format(name, "format", field, &value)?;
        }
        if let Some(value) =
            optional_pane_status_string(object.get("compact_format"), name, "compact_format")?
        {
            definition.compact_format =
                parse_pane_status_format(name, "compact_format", field, &value)?;
        }
        if let Some(value) = object.get("when") {
            let values = value.as_array().ok_or_else(|| {
                MezError::config(format!(
                    "frames.pane.pills.{name}.when must be a string array"
                ))
            })?;
            definition.when = values
                .iter()
                .map(|value| {
                    let value = value.as_str().ok_or_else(|| {
                        MezError::config(format!(
                            "frames.pane.pills.{name}.when must be a string array"
                        ))
                    })?;
                    PaneStatusCondition::parse(value).ok_or_else(|| {
                        MezError::config(format!(
                            "frames.pane.pills.{name}.when contains unsupported condition `{value}`"
                        ))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            validate_pane_status_conditions(name, &definition.when)?;
        }
        if object.contains_key("min_width") {
            definition.min_width =
                pane_status_optional_width(object.get("min_width"), name, "min_width")?;
        }
        if object.contains_key("max_width") {
            definition.max_width =
                pane_status_optional_width(object.get("max_width"), name, "max_width")?;
        }
        if definition
            .min_width
            .zip(definition.max_width)
            .is_some_and(|(minimum, maximum)| minimum > maximum)
        {
            return Err(MezError::config(format!(
                "frames.pane.pills.{name}.min_width must not exceed max_width"
            )));
        }
        if let Some(value) = object.get("priority") {
            let priority = value
                .as_u64()
                .filter(|value| *value <= 100)
                .ok_or_else(|| {
                    MezError::config(format!(
                        "frames.pane.pills.{name}.priority must be an integer from 0 to 100"
                    ))
                })?;
            definition.priority = u8::try_from(priority).map_err(|_| {
                MezError::config(format!(
                    "frames.pane.pills.{name}.priority must be an integer from 0 to 100"
                ))
            })?;
        }
        if let Some(value) = optional_pane_status_string(object.get("style"), name, "style")? {
            definition.style = PaneStatusStyle::parse(&value).ok_or_else(|| {
                MezError::config(format!(
                    "frames.pane.pills.{name}.style `{value}` is not a supported pane status style"
                ))
            })?;
        }
        if let Some(value) = optional_pane_status_string(object.get("on_click"), name, "on_click")?
        {
            definition.action = match value.as_str() {
                "none" => PaneStatusAction::None,
                "builtin" => field
                    .builtin_action()
                    .map(PaneStatusAction::Builtin)
                    .ok_or_else(|| {
                        MezError::config(format!(
                            "frames.pane.pills.{name}.on_click cannot be builtin because its source has no built-in action"
                        ))
                    })?,
                terminal if terminal.starts_with("terminal:") => {
                    let command = terminal.trim_start_matches("terminal:").trim();
                    PaneStatusAction::Terminal {
                        actions: compile_owner_targeted_terminal_actions(name, command)?,
                        origin: pane_status_provider_origin(effective, layers, name, "on_click"),
                    }
                }
                agent if agent.starts_with("agent:/") && agent.len() > "agent:/".len() => {
                    let command = agent.trim_start_matches("agent:");
                    validate_pane_status_agent_action(name, command)?;
                    PaneStatusAction::Agent {
                        command: command.to_string(),
                        origin: pane_status_provider_origin(effective, layers, name, "on_click"),
                    }
                }
                _ => {
                    return Err(MezError::config(format!(
                        "frames.pane.pills.{name}.on_click must be builtin, none, terminal:<owner-targeted command>, or agent:/<command>"
                    )));
                }
            };
        }
        config.pills.insert(name.clone(), definition);
    }
    Ok(config)
}

fn pane_status_provider_origin(
    effective: Option<&EffectiveConfig>,
    layers: &[ConfigLayer],
    name: &str,
    key: &str,
) -> Option<PaneStatusProviderOrigin> {
    let path = format!("frames.pane.pills.{name}.{key}");
    let layer_name = effective?.source_for(&path)?;
    let layer = layers.iter().find(|layer| layer.name == layer_name)?;
    let scope = match layer.scope {
        ConfigScope::Primary => PaneStatusProviderScope::Primary,
        ConfigScope::ProjectOverlay => PaneStatusProviderScope::ProjectOverlay,
        ConfigScope::LiveOverride => PaneStatusProviderScope::LiveOverride,
    };
    Some(PaneStatusProviderOrigin {
        layer_name: layer.name.clone(),
        scope,
        path: layer.path.as_ref().map(|path| path.display().to_string()),
        trusted: layer.trusted,
    })
}

fn validate_pane_status_agent_action(name: &str, command: &str) -> Result<()> {
    let invocation = parse_slash_command(command)
        .map_err(|error| {
            MezError::config(format!(
                "frames.pane.pills.{name}.on_click agent action is invalid: {error}"
            ))
        })?
        .ok_or_else(|| {
            MezError::config(format!(
                "frames.pane.pills.{name}.on_click agent action must be a slash command"
            ))
        })?;
    if !matches!(invocation.name.as_str(), "plan" | "stop") {
        return Err(MezError::config(format!(
            "frames.pane.pills.{name}.on_click agent action supports only /plan and /stop"
        )));
    }
    Ok(())
}

fn optional_pane_status_string(
    value: Option<&Value>,
    name: &str,
    key: &str,
) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        MezError::config(format!("frames.pane.pills.{name}.{key} must be a string"))
    })?;
    if value.chars().any(char::is_control) {
        return Err(MezError::config(format!(
            "frames.pane.pills.{name}.{key} must not contain control characters"
        )));
    }
    Ok(Some(value.to_string()))
}

fn positive_pane_status_u64(
    value: Option<&Value>,
    name: &str,
    key: &str,
    default: u64,
) -> Result<u64> {
    match value {
        None => Ok(default),
        Some(value) => value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
            MezError::config(format!(
                "frames.pane.pills.{name}.{key} must be a positive integer"
            ))
        }),
    }
}

fn parse_pane_status_empty_behavior(
    value: Option<&Value>,
    name: &str,
) -> Result<PaneStatusProviderEmptyBehavior> {
    match value.and_then(Value::as_str).unwrap_or("hide") {
        "hide" => Ok(PaneStatusProviderEmptyBehavior::Hide),
        "show_empty" => Ok(PaneStatusProviderEmptyBehavior::ShowEmpty),
        "keep_previous" => Ok(PaneStatusProviderEmptyBehavior::KeepPrevious),
        _ => Err(MezError::config(format!(
            "frames.pane.pills.{name}.empty_behavior must be hide, show_empty, or keep_previous"
        ))),
    }
}

fn parse_pane_status_error_behavior(
    value: Option<&Value>,
    name: &str,
) -> Result<PaneStatusProviderErrorBehavior> {
    match value.and_then(Value::as_str).unwrap_or("hide") {
        "hide" => Ok(PaneStatusProviderErrorBehavior::Hide),
        "show_error" => Ok(PaneStatusProviderErrorBehavior::ShowError),
        "keep_previous" => Ok(PaneStatusProviderErrorBehavior::KeepPrevious),
        _ => Err(MezError::config(format!(
            "frames.pane.pills.{name}.error_behavior must be hide, show_error, or keep_previous"
        ))),
    }
}

fn compile_owner_targeted_terminal_actions(
    name: &str,
    command: &str,
) -> Result<Vec<PaneStatusTerminalAction>> {
    if command.is_empty() {
        return Err(MezError::config(format!(
            "frames.pane.pills.{name}.on_click terminal action must not be empty"
        )));
    }
    let invocations = parse_command_sequence(command).map_err(|error| {
        MezError::config(format!(
            "frames.pane.pills.{name}.on_click terminal action is invalid: {error}"
        ))
    })?;
    let mut actions = Vec::with_capacity(invocations.len());
    for invocation in invocations {
        let command = PaneStatusTerminalCommand::parse(&invocation.name).ok_or_else(|| {
            MezError::config(format!(
                "frames.pane.pills.{name}.on_click terminal command `{}` is not a supported pane-targeted action",
                invocation.name
            ))
        })?;
        let mut arguments = Vec::with_capacity(invocation.args.len().saturating_sub(2));
        let mut target_seen = false;
        let mut index = 0;
        while index < invocation.args.len() {
            let argument = &invocation.args[index];
            if argument == "-t" {
                let target = invocation.args.get(index + 1).map(String::as_str);
                if target_seen || target != Some("{pane}") {
                    return Err(MezError::config(format!(
                        "frames.pane.pills.{name}.on_click terminal actions must contain exactly one -t {{pane}} target"
                    )));
                }
                target_seen = true;
                index += 2;
                continue;
            }
            if argument.contains("{pane}") {
                return Err(MezError::config(format!(
                    "frames.pane.pills.{name}.on_click may use {{pane}} only as the -t target"
                )));
            }
            arguments.push(argument.clone());
            index += 1;
        }
        if !target_seen {
            return Err(MezError::config(format!(
                "frames.pane.pills.{name}.on_click terminal actions must contain exactly one -t {{pane}} target"
            )));
        }
        actions.push(PaneStatusTerminalAction { command, arguments });
    }
    Ok(actions)
}

fn parse_pane_status_format(
    name: &str,
    key: &str,
    field: PaneStatusField,
    value: &str,
) -> Result<PaneStatusFormat> {
    let format = PaneStatusFormat::parse(value).ok_or_else(|| {
        MezError::config(format!(
            "frames.pane.pills.{name}.{key} must be full, short, or percent"
        ))
    })?;
    if format == PaneStatusFormat::Percent && !field.supports_percent() {
        return Err(MezError::config(format!(
            "frames.pane.pills.{name}.{key} percent format is not supported for `{}`",
            field.as_str()
        )));
    }
    Ok(format)
}

fn pane_status_optional_width(
    value: Option<&Value>,
    name: &str,
    key: &str,
) -> Result<Option<usize>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let width = value
        .as_u64()
        .filter(|width| (1..=4096).contains(width))
        .and_then(|width| usize::try_from(width).ok())
        .ok_or_else(|| {
            MezError::config(format!(
                "frames.pane.pills.{name}.{key} must be an integer from 1 to 4096"
            ))
        })?;
    Ok(Some(width))
}

fn validate_pane_status_conditions(name: &str, conditions: &[PaneStatusCondition]) -> Result<()> {
    for (left, right) in [
        (
            PaneStatusCondition::AgentView,
            PaneStatusCondition::ShellView,
        ),
        (PaneStatusCondition::Focused, PaneStatusCondition::Unfocused),
        (PaneStatusCondition::Busy, PaneStatusCondition::Idle),
    ] {
        if conditions.contains(&left) && conditions.contains(&right) {
            return Err(MezError::config(format!(
                "frames.pane.pills.{name}.when contains contradictory conditions"
            )));
        }
    }
    Ok(())
}

/// Runs the runtime window frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_window_frame_position_from_config(
    root: &Value,
) -> Result<TerminalFramePosition> {
    runtime_frame_position_from_config(root, "window", TerminalFramePosition::Bottom)
}

/// Runs the runtime pane frame position from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_pane_frame_position_from_config(
    root: &Value,
) -> Result<TerminalFramePosition> {
    runtime_frame_position_from_config(root, "pane", TerminalFramePosition::Top)
}

/// Runs the runtime window frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_window_frame_style_from_config(root: &Value) -> Result<TerminalFrameStyle> {
    runtime_frame_style_from_config(root, "window")
}

/// Runs the runtime pane frame style from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_pane_frame_style_from_config(root: &Value) -> Result<TerminalFrameStyle> {
    runtime_frame_style_from_config(root, "pane")
}

/// Runs the runtime window frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_window_frame_visible_fields_from_config(root: &Value) -> Result<Vec<String>> {
    runtime_frame_visible_fields_from_config(
        root,
        "window",
        crate::host::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS,
    )
}

/// Runs the runtime pane frame visible fields from config operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_pane_frame_visible_fields_from_config(root: &Value) -> Result<Vec<String>> {
    runtime_frame_visible_fields_from_config(
        root,
        "pane",
        crate::host::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS,
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
pub(crate) fn runtime_key_bindings_from_config(root: &Value) -> Result<KeyBindings> {
    let (_, mut bindings, _) = runtime_active_key_preset(root)?;
    let Some(keys) = runtime_json_object(root, "keys") else {
        return Ok(bindings);
    };
    bindings.escape = runtime_key_binding_value(keys, "escape", bindings.escape)?;
    bindings.split_vertical =
        runtime_optional_key_binding_value(keys, "split_vertical", bindings.split_vertical)?;
    bindings.split_horizontal =
        runtime_optional_key_binding_value(keys, "split_horizontal", bindings.split_horizontal)?;
    bindings.new_window =
        runtime_optional_key_binding_value(keys, "new_window", bindings.new_window)?;
    bindings.new_group = runtime_optional_key_binding_value(keys, "new_group", bindings.new_group)?;
    bindings.agent_shell =
        runtime_optional_key_binding_value(keys, "agent_shell", bindings.agent_shell)?;
    bindings.edit_prompt =
        runtime_optional_key_binding_value(keys, "edit_prompt", bindings.edit_prompt)?;
    bindings.focus_up = runtime_optional_key_binding_value(keys, "focus_up", bindings.focus_up)?;
    bindings.focus_down =
        runtime_optional_key_binding_value(keys, "focus_down", bindings.focus_down)?;
    bindings.focus_left =
        runtime_optional_key_binding_value(keys, "focus_left", bindings.focus_left)?;
    bindings.focus_right =
        runtime_optional_key_binding_value(keys, "focus_right", bindings.focus_right)?;
    bindings.focus_previous_window = runtime_optional_key_binding_value(
        keys,
        "focus_previous_window",
        bindings.focus_previous_window,
    )?;
    bindings.focus_next_window =
        runtime_optional_key_binding_value(keys, "focus_next_window", bindings.focus_next_window)?;
    bindings.focus_previous_group = runtime_optional_key_binding_value(
        keys,
        "focus_previous_group",
        bindings.focus_previous_group,
    )?;
    bindings.focus_next_group =
        runtime_optional_key_binding_value(keys, "focus_next_group", bindings.focus_next_group)?;
    for action in ConfigurableKeyAction::ALL {
        if keys.contains_key(action.config_field()) {
            bindings.replace_default_prefix_action(action);
        }
    }
    Ok(bindings)
}

/// Runs the runtime key binding value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_key_binding_value(
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
pub(crate) fn runtime_optional_key_binding_value(
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
pub(crate) fn runtime_command_bindings_from_effective(
    root: &Value,
    effective: &EffectiveConfig,
) -> Result<BTreeMap<KeyChord, RuntimeCommandBinding>> {
    let mut bindings = BTreeMap::new();
    let (preset_name, _, preset) = runtime_active_key_preset(root)?;
    for (config_key, command) in preset.command_bindings {
        let (chord, notation) = runtime_chord_from_binding_config_key(&config_key)?;
        parse_command_sequence(&command).map_err(|error| {
            MezError::config(format!(
                "key preset `{preset_name}` command binding `{config_key}` is invalid: {error}"
            ))
        })?;
        bindings.insert(
            chord,
            RuntimeCommandBinding {
                notation,
                command,
                source_layer: format!("key-preset:{preset_name}"),
            },
        );
    }
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

/// Rejects a configured prompt-edit suffix that would shadow another prefix
/// action or a user-defined prefix command binding.
pub(crate) fn runtime_validate_key_binding_collisions(
    bindings: &KeyBindings,
    command_bindings: &BTreeMap<KeyChord, RuntimeCommandBinding>,
) -> Result<()> {
    let Some(edit_prompt) = bindings.edit_prompt else {
        return Ok(());
    };
    let mut bindings_without_edit_prompt = bindings.clone();
    bindings_without_edit_prompt.edit_prompt = None;
    if classify_prefix_binding(edit_prompt, &bindings_without_edit_prompt).is_some() {
        return Err(MezError::config(
            "keys.edit_prompt conflicts with a built-in prefix binding",
        ));
    }
    if command_bindings.contains_key(&edit_prompt) {
        return Err(MezError::config(
            "keys.edit_prompt conflicts with a configured prefix command binding",
        ));
    }
    Ok(())
}

/// Runs the runtime chord from binding config key operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(crate) fn runtime_chord_from_binding_config_key(
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
pub(crate) fn runtime_decode_binding_config_key(encoded: &str) -> Result<String> {
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

#[cfg(test)]
mod pane_status_preset_tests {
    use super::*;
    use crate::config::{ConfigFormat, DEFAULT_CONFIG_TOML, parse_config_json_value};
    use crate::host::terminal::PaneAgentStatusField;

    fn parse(text: &str) -> PaneStatusConfig {
        let root: Value = toml::from_str::<toml::Value>(text)
            .unwrap()
            .try_into()
            .unwrap();
        runtime_pane_status_config_from_config(&root).unwrap()
    }

    /// Each preset expands to a distinct typed composition before any explicit overrides.
    #[test]
    fn pane_status_presets_expand_to_typed_defaults() {
        let standard = parse("[frames.pane]\nstatus_preset = \"standard\"\n");
        let minimal = parse("[frames.pane]\nstatus_preset = \"minimal\"\n");
        let agent = parse("[frames.pane]\nstatus_preset = \"agent-focused\"\n");
        let full = parse("[frames.pane]\nstatus_preset = \"full-controls\"\n");

        assert_eq!(standard.preset_name(), "standard");
        assert_eq!(minimal.preset_name(), "minimal");
        assert_eq!(agent.preset_name(), "agent-focused");
        assert_eq!(full.preset_name(), "full-controls");
        assert_ne!(minimal.right_status, standard.right_status);
        assert!(agent.pills.contains_key("inactive"));
        assert!(agent.pills.contains_key("thinking"));
        assert!(agent.pills.contains_key("preset"));
        assert!(full.pills.contains_key("thinking"));
        assert!(full.pills.contains_key("preset"));
    }

    /// A generated configuration leaves rails inherited, so changing only the
    /// selected preset changes the effective pane-status composition.
    #[test]
    fn generated_config_switches_pane_status_rails_with_only_the_preset() {
        let standard_root = parse_config_json_value(ConfigFormat::Toml, DEFAULT_CONFIG_TOML)
            .expect("generated configuration should parse");
        let standard = runtime_pane_status_config_from_config(&standard_root).unwrap();
        let mut minimal_root = standard_root;
        minimal_root["frames"]["pane"]["status_preset"] = Value::String("minimal".to_string());
        let minimal = runtime_pane_status_config_from_config(&minimal_root).unwrap();

        assert_eq!(standard.preset_name(), "standard");
        assert_eq!(minimal.preset_name(), "minimal");
        assert_ne!(standard.right_status, minimal.right_status);
        assert_eq!(
            minimal.right_status,
            "#{pane.pwd} #{agent.status} #{history.position}"
        );
    }

    /// Explicit empty/custom rails and scalar layout values override preset defaults verbatim.
    #[test]
    fn pane_status_explicit_values_override_preset_defaults() {
        let config = parse(
            "[frames.pane]\nstatus_preset = \"full-controls\"\nleft_status = \"\"\nright_status = \"#{pane.status}\"\noverflow = \"hide\"\ntitle_min_width = 17\n",
        );

        assert_eq!(config.left_status, "");
        assert_eq!(config.right_status, "#{pane.status}");
        assert_eq!(config.overflow, PaneStatusOverflowPolicy::Hide);
        assert_eq!(config.title_min_width, 17);
    }

    /// Named preset pills merge by stable id so one explicit leaf does not need to restate source.
    #[test]
    fn pane_status_named_pill_leaf_merges_with_preset_definition() {
        let config = parse(
            "[frames.pane]\nstatus_preset = \"full-controls\"\n[frames.pane.pills.model]\nlabel = \"Runtime\"\npriority = 99\n",
        );
        let model = &config.pills["model"];

        assert_eq!(model.field, PaneStatusField::AgentModel);
        assert_eq!(model.label.as_deref(), Some("Runtime"));
        assert_eq!(model.priority, 99);
        assert!(matches!(model.action, PaneStatusAction::Builtin(_)));
    }

    /// An explicit source leaf replaces only that leaf while the remaining
    /// stable-ID metadata continues to inherit from the selected preset.
    #[test]
    fn pane_status_named_pill_source_leaf_preserves_preset_metadata() {
        let config = parse(
            "[frames.pane]\nstatus_preset = \"agent-focused\"\n[frames.pane.pills.model]\nfield = \"agent.reasoning\"\n",
        );
        let model = &config.pills["model"];

        assert_eq!(model.field, PaneStatusField::AgentReasoning);
        assert!(model.when.contains(&PaneStatusCondition::Focused));
        assert!(matches!(
            model.action,
            PaneStatusAction::Builtin(PaneAgentStatusField::Reasoning)
        ));
    }
}
