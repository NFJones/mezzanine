//! Runtime command helpers for keybinding display and mutation commands.
//!
//! This module owns keybinding list, bind, unbind, source-selection, and default
//! prefix binding display helpers. Keeping this command family isolated reduces
//! the size of the general command-support facade without changing the runtime
//! command names.

use super::*;
use crate::command::command_help_display_with_key_bindings;

/// Runs the runtime help display operation for this subsystem.
///
/// The function reuses the baseline help prose while substituting the effective
/// runtime key binding table so `help` matches the configured live bindings.
pub(in crate::runtime) fn runtime_command_help_display(
    service: &RuntimeSessionService,
) -> Result<String> {
    let key_bindings = runtime_list_key_bindings_display(service)?;
    Ok(command_help_display_with_key_bindings(&key_bindings))
}

/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_list_key_bindings_display(
    service: &RuntimeSessionService,
) -> Result<String> {
    let effective = compose_effective_config(&service.config_layers)?;
    let prefix = key_chord_notation(service.key_bindings.escape);
    let mut rows = Vec::new();
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.split_vertical,
        runtime_key_source(&effective, "keys.split_vertical"),
        "split-window",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.split_horizontal,
        runtime_key_source(&effective, "keys.split_horizontal"),
        "split-window -h",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.new_window,
        runtime_key_source(&effective, "keys.new_window"),
        "new-window",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.new_group,
        runtime_key_source(&effective, "keys.new_group"),
        "new-group",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.agent_shell,
        runtime_key_source(&effective, "keys.agent_shell"),
        "agent-shell",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_up,
        runtime_key_source(&effective, "keys.focus_up"),
        "select-pane -U",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_down,
        runtime_key_source(&effective, "keys.focus_down"),
        "select-pane -D",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_left,
        runtime_key_source(&effective, "keys.focus_left"),
        "select-pane -L",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_right,
        runtime_key_source(&effective, "keys.focus_right"),
        "select-pane -R",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_previous_window,
        runtime_key_source(&effective, "keys.focus_previous_window"),
        "previous-window",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_next_window,
        runtime_key_source(&effective, "keys.focus_next_window"),
        "next-window",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_previous_group,
        runtime_key_source(&effective, "keys.focus_previous_group"),
        "previous-group",
    );
    runtime_push_optional_key_binding_row(
        &mut rows,
        service.key_bindings.focus_next_group,
        runtime_key_source(&effective, "keys.focus_next_group"),
        "next-group",
    );

    for (chord, command) in runtime_default_prefix_bindings(service.key_bindings.escape) {
        if service.command_bindings.contains_key(&chord) {
            continue;
        }
        rows.push(RuntimeKeyBindingDisplayRow {
            key: format!("{prefix} {}", key_chord_notation(chord)),
            source: runtime_key_source(&effective, "keys.escape").to_string(),
            command: command.to_string(),
        });
    }
    for binding in service.command_bindings.values() {
        rows.push(RuntimeKeyBindingDisplayRow {
            key: format!("{prefix} {}", binding.notation),
            source: binding.source_layer.clone(),
            command: binding.command.clone(),
        });
    }
    Ok(runtime_key_binding_rows_display(&rows))
}

/// Runs the runtime bind key command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_bind_key_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let (key, command) = bind_key_args(invocation)?;
    parse_command_sequence(&command)?;
    let chord = KeyChord::parse(key)?;
    let notation = key_chord_notation(chord);
    let config_key = binding_config_key(&notation);
    let mutation = ConfigMutation {
        path: format!("keys.command_bindings.{config_key}"),
        operation: ConfigMutationOperation::Set(ConfigMutationValue::String(command.clone())),
    };
    let plan = runtime_plan_live_override_mutation(service, mutation)?;
    runtime_store_live_override_plan(service, &plan.text);
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:bind-key", &report),
    )?;
    Ok(format!(
        "key={notation}:config_key={config_key}:command={}:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}",
        json_escape(&command),
        plan.changed,
        plan.reload_required
    ))
}

/// Runs the runtime unbind key command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_unbind_key_command(
    service: &mut RuntimeSessionService,
    invocation: &CommandInvocation,
) -> Result<String> {
    let key = runtime_positional_args(invocation)
        .first()
        .copied()
        .ok_or_else(|| MezError::invalid_args("unbind-key requires a key"))?;
    let chord = KeyChord::parse(key)?;
    let notation = key_chord_notation(chord);
    let config_key = binding_config_key(&notation);
    let mutation = ConfigMutation {
        path: format!("keys.command_bindings.{config_key}"),
        operation: ConfigMutationOperation::Unset,
    };
    let plan = runtime_plan_live_override_mutation(service, mutation)?;
    runtime_store_live_override_plan(service, &plan.text);
    let report = service.apply_runtime_config_layers()?;
    service.append_lifecycle_event(
        EventKind::ConfigChanged,
        runtime_config_apply_event_payload("terminal/command:unbind-key", &report),
    )?;
    Ok(format!(
        "key={notation}:config_key={config_key}:removed=true:changed={}:reload_required={}:source=runtime-config:layer={TERMINAL_COMMAND_LIVE_OVERRIDE_LAYER}",
        plan.changed, plan.reload_required
    ))
}

/// Runs the runtime key source operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_key_source<'a>(
    effective: &'a crate::config::EffectiveConfig,
    path: &str,
) -> &'a str {
    effective.source_for(path).unwrap_or("default")
}

/// Carries one effective key binding row before alignment.
///
/// The type keeps display data structured so command output can align columns
/// without reparsing display strings.
struct RuntimeKeyBindingDisplayRow {
    /// The display notation for the key chord or chord sequence.
    key: String,
    /// The configuration source for the binding.
    source: String,
    /// The command executed by the binding.
    command: String,
}

/// Adds an effective direct key binding row when the binding is enabled.
///
/// # Parameters
/// - `rows`: The table rows being constructed.
/// - `chord`: The optional direct key chord.
/// - `source`: The source label for the binding.
/// - `command`: The command executed by the binding.
fn runtime_push_optional_key_binding_row(
    rows: &mut Vec<RuntimeKeyBindingDisplayRow>,
    chord: Option<KeyChord>,
    source: &str,
    command: &str,
) {
    if let Some(chord) = chord {
        rows.push(RuntimeKeyBindingDisplayRow {
            key: key_chord_notation(chord),
            source: source.to_string(),
            command: command.to_string(),
        });
    }
}

/// Renders effective key binding rows with aligned columns.
///
/// # Parameters
/// - `rows`: The key binding rows to display.
fn runtime_key_binding_rows_display(rows: &[RuntimeKeyBindingDisplayRow]) -> String {
    let key_width = rows
        .iter()
        .map(|row| row.key.len())
        .max()
        .unwrap_or("key".len())
        .max("key".len());
    let source_width = rows
        .iter()
        .map(|row| row.source.len())
        .max()
        .unwrap_or("source".len())
        .max("source".len());
    std::iter::once(format!(
        "{:<key_width$}  {:<source_width$}  command",
        "key", "source"
    ))
    .chain(rows.iter().map(|row| {
        format!(
            "{:<key_width$}  {:<source_width$}  {}",
            row.key,
            json_escape(&row.source),
            json_escape(&row.command)
        )
    }))
    .collect::<Vec<_>>()
    .join("\n")
}

/// Runs the runtime default prefix bindings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_default_prefix_bindings(
    escape: KeyChord,
) -> Vec<(KeyChord, &'static str)> {
    vec![
        (escape, "send-prefix"),
        (KeyChord::new(KeyCode::Char(':')), "command-prompt"),
        (KeyChord::new(KeyCode::Char('?')), "list-keys"),
        (KeyChord::new(KeyCode::Char('d')), "detach-client"),
        (KeyChord::new(KeyCode::Char('D')), "choose-client"),
        (KeyChord::new(KeyCode::Char('G')), "choose-group"),
        (KeyChord::new(KeyCode::Char('c')), "new-window"),
        (KeyChord::new(KeyCode::Char(',')), "rename-window"),
        (KeyChord::new(KeyCode::Char('&')), "kill-window --force"),
        (KeyChord::new(KeyCode::Char('w')), "choose-window"),
        (KeyChord::new(KeyCode::Char('n')), "next-window"),
        (KeyChord::new(KeyCode::Char('p')), "previous-window"),
        (KeyChord::new(KeyCode::Char('l')), "last-window"),
        (KeyChord::new(KeyCode::Char('0')), "select-window -t 0"),
        (KeyChord::new(KeyCode::Char('1')), "select-window -t 1"),
        (KeyChord::new(KeyCode::Char('2')), "select-window -t 2"),
        (KeyChord::new(KeyCode::Char('3')), "select-window -t 3"),
        (KeyChord::new(KeyCode::Char('4')), "select-window -t 4"),
        (KeyChord::new(KeyCode::Char('5')), "select-window -t 5"),
        (KeyChord::new(KeyCode::Char('6')), "select-window -t 6"),
        (KeyChord::new(KeyCode::Char('7')), "select-window -t 7"),
        (KeyChord::new(KeyCode::Char('8')), "select-window -t 8"),
        (KeyChord::new(KeyCode::Char('9')), "select-window -t 9"),
        (
            KeyChord::new(KeyCode::Char('\'')),
            "select-window -t prompt",
        ),
        (KeyChord::new(KeyCode::Char('.')), "move-window -t prompt"),
        (KeyChord::new(KeyCode::Char('%')), "split-window"),
        (KeyChord::new(KeyCode::Char('"')), "split-window -h"),
        (KeyChord::new(KeyCode::Up), "select-pane -U"),
        (KeyChord::new(KeyCode::Down), "select-pane -D"),
        (KeyChord::new(KeyCode::Left), "select-pane -L"),
        (KeyChord::new(KeyCode::Right), "select-pane -R"),
        (KeyChord::new(KeyCode::Char('o')), "select-pane -t next"),
        (KeyChord::new(KeyCode::Char(';')), "last-pane"),
        (KeyChord::new(KeyCode::Char('q')), "display-panes"),
        (KeyChord::new(KeyCode::Char('z')), "resize-pane -Z"),
        (KeyChord::new(KeyCode::Char(' ')), "next-layout"),
        (KeyChord::new(KeyCode::Char('x')), "kill-pane --force"),
        (KeyChord::new(KeyCode::Char('!')), "break-pane"),
        (KeyChord::new(KeyCode::Char('{')), "swap-pane -U"),
        (KeyChord::new(KeyCode::Char('}')), "swap-pane -D"),
        (KeyChord::new(KeyCode::PageUp), "copy-mode -u"),
        (KeyChord::new(KeyCode::Char('[')), "copy-mode"),
        (KeyChord::new(KeyCode::Char(']')), "paste-buffer"),
        (KeyChord::new(KeyCode::Char('#')), "list-buffers"),
        (KeyChord::new(KeyCode::Char('=')), "choose-buffer"),
        (KeyChord::new(KeyCode::Char('-')), "delete-buffer"),
        (KeyChord::new(KeyCode::Char('O')), "choose-observer"),
        (KeyChord::new(KeyCode::Char('~')), "show-messages"),
    ]
}
