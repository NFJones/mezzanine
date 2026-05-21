//! Command Parser implementation.
//!
//! This module owns the command parser boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{CommandInvocation, MezError, QuoteState, Result};

// Command sequence parsing and tokenization.

/// Runs the parse command sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_command_sequence(input: &str) -> Result<Vec<CommandInvocation>> {
    let segments = split_semicolon_sequence(input)?;
    let mut commands = Vec::new();

    for segment in segments {
        let tokens = tokenize_command(&segment)?;
        if tokens.is_empty() {
            continue;
        }
        commands.push(CommandInvocation {
            name: tokens[0].clone(),
            args: tokens[1..].to_vec(),
        });
    }

    if commands.is_empty() {
        return Err(MezError::invalid_args(
            "command input did not contain a command",
        ));
    }

    Ok(commands)
}
/// Runs the split semicolon sequence operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_semicolon_sequence(input: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if quote != QuoteState::Single => {
                escaped = true;
                current.push(ch);
            }
            '\'' if quote == QuoteState::None => {
                quote = QuoteState::Single;
                current.push(ch);
            }
            '\'' if quote == QuoteState::Single => {
                quote = QuoteState::None;
                current.push(ch);
            }
            '"' if quote == QuoteState::None => {
                quote = QuoteState::Double;
                current.push(ch);
            }
            '"' if quote == QuoteState::Double => {
                quote = QuoteState::None;
                current.push(ch);
            }
            ';' if quote == QuoteState::None => {
                segments.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        return Err(MezError::invalid_args("command sequence ends with escape"));
    }
    if quote != QuoteState::None {
        return Err(MezError::invalid_args(
            "unterminated quoted command argument",
        ));
    }

    segments.push(current.trim().to_string());
    Ok(segments)
}

/// Runs the tokenize command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn tokenize_command(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' if quote != QuoteState::Single => {
                escaped = true;
            }
            '\'' if quote == QuoteState::None => {
                quote = QuoteState::Single;
            }
            '\'' if quote == QuoteState::Single => {
                quote = QuoteState::None;
            }
            '"' if quote == QuoteState::None => {
                quote = QuoteState::Double;
            }
            '"' if quote == QuoteState::Double => {
                quote = QuoteState::None;
            }
            ch if ch.is_whitespace() && quote == QuoteState::None => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped {
        return Err(MezError::invalid_args("command ends with escape"));
    }
    if quote != QuoteState::None {
        return Err(MezError::invalid_args(
            "unterminated quoted command argument",
        ));
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    Ok(tokens)
}
