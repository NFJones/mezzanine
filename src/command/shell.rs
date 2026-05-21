//! Command Shell implementation.
//!
//! This module owns the command shell boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{CommandInvocation, MezError, Result};

// Explicit shell-command argument handling.

/// Runs the flag value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

/// Runs the positional args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn positional_args(invocation: &CommandInvocation) -> Vec<&str> {
    positional_args_from_slice(&invocation.args)
}

/// Runs the positional args before double dash operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn positional_args_before_double_dash(invocation: &CommandInvocation) -> Vec<&str> {
    let end = invocation
        .args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(invocation.args.len());
    positional_args_from_slice(&invocation.args[..end])
}

/// Runs the positional args from slice operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn positional_args_from_slice(args: &[String]) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        if matches!(
            arg,
            "-t" | "-s"
                | "-c"
                | "-n"
                | "--name"
                | "-x"
                | "--columns"
                | "-y"
                | "--rows"
                | "--percent"
                | "--axis"
                | "--delta"
                | "--edge"
                | "--amount"
                | "--scope"
                | "--match"
                | "--exact-sha256"
                | "--shell-classification"
                | "--justification"
                | "--reason"
                | "--epoch"
                | "--content"
        ) {
            index += 2;
            continue;
        }
        if arg.starts_with("--") {
            index += 1;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        values.push(arg);
        index += 1;
    }
    values
}

/// Runs the explicit shell command flag operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn explicit_shell_command_flag(
    invocation: &CommandInvocation,
) -> Result<Option<String>> {
    match flag_value(&invocation.args, "--shell-command")
        .or_else(|| flag_value(&invocation.args, "--command"))
    {
        Some(command) if command.trim().is_empty() => Err(MezError::invalid_args(
            "pane shell command must not be empty",
        )),
        Some(command) => Ok(Some(command.to_string())),
        None => Ok(None),
    }
}

/// Runs the shell command after double dash operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_command_after_double_dash(
    invocation: &CommandInvocation,
) -> Result<Option<String>> {
    let Some(index) = invocation.args.iter().position(|arg| arg == "--") else {
        return Ok(None);
    };
    shell_command_from_words(
        invocation.args[index.saturating_add(1)..]
            .iter()
            .map(String::as_str)
            .collect(),
    )
}

/// Runs the shell command from words operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_command_from_words(words: Vec<&str>) -> Result<Option<String>> {
    if words.is_empty() {
        return Ok(None);
    }
    let command = shell_join_words(&words);
    if command.trim().is_empty() {
        return Err(MezError::invalid_args(
            "pane shell command must not be empty",
        ));
    }
    Ok(Some(command))
}

/// Runs the shell join words operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_join_words(words: &[&str]) -> String {
    words
        .iter()
        .map(|word| shell_quote_word(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs the shell quote word operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_quote_word(word: &str) -> String {
    if !word.is_empty()
        && word.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
                )
        })
    {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Carries Quote State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuoteState {
    /// Represents the None case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    None,
    /// Represents the Single case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Single,
    /// Represents the Double case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Double,
}
