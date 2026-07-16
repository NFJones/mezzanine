//! Slash-command parsing helpers for runtime agent commands.
//!
//! The parent command module executes live slash commands against runtime
//! state. This child module keeps argument normalization, validation, and
//! small command-invocation adapters together so unrelated command execution
//! paths do not carry low-level parsing helpers directly.

use super::super::{CommandInvocation, MezError, Result, parse_command_sequence};

/// Runs the runtime single mode arg operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_single_mode_arg(args: &str, command: &str, default: &str) -> Result<String> {
    let values = args.split_whitespace().collect::<Vec<_>>();
    if values.len() > 1 {
        return Err(MezError::invalid_args(format!(
            "{command} slash command accepts at most one argument"
        )));
    }
    Ok(values.first().copied().unwrap_or(default).to_string())
}

/// Runs the validate agent personality operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_agent_personality(value: &str) -> Result<()> {
    if value.len() > 64 {
        return Err(MezError::invalid_args(
            "personality slash command style must be 64 bytes or fewer",
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(MezError::invalid_args(
            "personality slash command style must not contain control characters",
        ));
    }
    Ok(())
}

/// Runs the runtime single permissions invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_single_permissions_invocation(args: &str) -> Result<CommandInvocation> {
    let trimmed = args.trim();
    let command = if trimmed.is_empty() {
        "permissions".to_string()
    } else {
        let (head, tail) = trimmed
            .split_once(char::is_whitespace)
            .map(|(head, tail)| (head, tail.trim()))
            .unwrap_or((trimmed, ""));
        match head {
            "list" | "rules" => "list-command-rules".to_string(),
            "allow" => format!("allow-command {tail}"),
            "deny" => format!("deny-command {tail}"),
            "prompt" => format!("prompt-command {tail}"),
            "remove" | "delete" => format!("remove-command-rule {tail}"),
            "bypass" => {
                if tail.is_empty() {
                    "bypass-approvals status".to_string()
                } else {
                    format!("bypass-approvals {tail}")
                }
            }
            _ => format!("permissions {trimmed}"),
        }
    };
    let invocations = parse_command_sequence(&command)?;
    let [invocation] = invocations.as_slice() else {
        return Err(MezError::invalid_args(
            "permissions slash command accepts only one policy command",
        ));
    };
    if !matches!(
        invocation.name.as_str(),
        "permissions"
            | "list-command-rules"
            | "allow-command"
            | "deny-command"
            | "prompt-command"
            | "remove-command-rule"
            | "bypass-approvals"
    ) {
        return Err(MezError::invalid_args(
            "permissions slash command can only execute policy commands",
        ));
    }
    Ok(invocation.clone())
}

/// Runs the runtime single approval invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_single_approval_invocation(args: &str) -> Result<CommandInvocation> {
    let command = if args.trim().is_empty() {
        "approval".to_string()
    } else {
        format!("approval {args}")
    };
    let invocations = parse_command_sequence(&command)?;
    let [invocation] = invocations.as_slice() else {
        return Err(MezError::invalid_args(
            "approval slash command accepts only one approval command",
        ));
    };
    if invocation.name != "approval" {
        return Err(MezError::invalid_args(
            "approval slash command can only execute approval",
        ));
    }
    Ok(invocation.clone())
}

/// Runs the runtime single rename window invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_single_rename_window_invocation(args: &str) -> Result<CommandInvocation> {
    let invocations = parse_command_sequence(&format!("rename-window {args}"))?;
    let [invocation] = invocations.as_slice() else {
        return Err(MezError::invalid_args(
            "title slash command accepts only one title value",
        ));
    };
    if invocation.name != "rename-window" || invocation.target_arg().is_some() {
        return Err(MezError::invalid_args(
            "title slash command can only rename the active window",
        ));
    }
    Ok(invocation.clone())
}

/// Runs the runtime agent init scaffold operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_init_scaffold() -> &'static str {
    "# Repository Guidelines\n\n\
## Project Structure\n\
- Document the main source, test, documentation, and generated-output directories.\n\n\
## Build, Test, and Development Commands\n\
- List the commands contributors should run before handing off changes.\n\n\
## Coding Style\n\
- Describe formatting, naming, review, and documentation expectations.\n\n\
## Security and Configuration\n\
- Note secret-handling rules, local overrides, generated files, and unsafe operations.\n"
}
