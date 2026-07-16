//! Product command outcome JSON projection and optional scalar helpers.

use super::{CommandOutcome, Path, json_escape};

/// Runs the runtime command outcomes json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_command_outcomes_json(outcomes: &[CommandOutcome]) -> String {
    let outcomes = outcomes
        .iter()
        .map(runtime_command_outcome_json)
        .collect::<Vec<_>>();
    format!(
        r#"{{"executed":{},"outcomes":[{}]}}"#,
        outcomes.len(),
        outcomes.join(",")
    )
}

/// Runs the runtime command outcome json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn runtime_command_outcome_json(outcome: &CommandOutcome) -> String {
    match outcome {
        CommandOutcome::Noop { command } => {
            format!(r#"{{"command":"{}","kind":"noop"}}"#, json_escape(command))
        }
        CommandOutcome::Mutated { command } => format!(
            r#"{{"command":"{}","kind":"mutated"}}"#,
            json_escape(command)
        ),
        CommandOutcome::MutatedWithPaneCommand {
            command,
            shell_command,
            start_directory,
        } => format!(
            r#"{{"command":"{}","kind":"mutated_with_pane_command","shell_command":"{}","start_directory":{}}}"#,
            json_escape(command),
            json_escape(shell_command),
            optional_string_json(start_directory.as_deref())
        ),
        CommandOutcome::Display { command, body } => format!(
            r#"{{"command":"{}","kind":"display","body":"{}"}}"#,
            json_escape(command),
            json_escape(body)
        ),
        CommandOutcome::LayoutSave { command, name } => format!(
            r#"{{"command":"{}","kind":"layout_save","name":{},"body":"runtime layout repository required"}}"#,
            json_escape(command),
            optional_string_json(name.as_deref())
        ),
        CommandOutcome::LayoutLoad { command, selector } => format!(
            r#"{{"command":"{}","kind":"layout_load","selector":{},"body":"runtime layout repository required"}}"#,
            json_escape(command),
            runtime_layout_load_selector_json(selector)
        ),
    }
}

/// Renders a layout load selector for runtime command JSON diagnostics.
fn runtime_layout_load_selector_json(selector: &crate::command::LayoutLoadSelector) -> String {
    match selector {
        crate::command::LayoutLoadSelector::Name(name) => {
            format!(r#"{{"kind":"name","name":"{}"}}"#, json_escape(name))
        }
        crate::command::LayoutLoadSelector::Latest => r#"{"kind":"latest"}"#.to_string(),
    }
}

/// Runs the optional string json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn optional_string_json(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the optional path json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(in crate::runtime) fn optional_path_json(value: Option<&Path>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(&value.to_string_lossy())))
        .unwrap_or_else(|| "null".to_string())
}
