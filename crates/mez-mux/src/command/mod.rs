//! Multiplexer command-language grammar.
//!
//! This module owns dependency-neutral command invocations, flag and
//! positional argument queries, quoting, tokenization, and semicolon sequence
//! parsing. Product command registries, dispatch, persistence, and error
//! projection remain in the composition crate.

use crate::{MuxError, Result};

pub mod plans;
pub mod presentation;

/// Parsed command name and ordered arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInvocation {
    /// Canonical or user-supplied command name.
    pub name: String,
    /// Ordered command arguments after tokenization.
    pub args: Vec<String>,
}

impl CommandInvocation {
    /// Returns command arguments that are not flags or flag values.
    pub fn positional_args(&self) -> Vec<&str> {
        positional_args_from_slice(&self.args)
    }

    /// Returns the value immediately following an arbitrary flag spelling.
    pub fn flag_value(&self, flag: &str) -> Option<&str> {
        flag_value(&self.args, flag)
    }

    /// Returns the value following `-t`, when present.
    pub fn target_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-t")
    }

    /// Returns the value following `-s`, when present.
    pub fn source_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-s")
    }

    /// Returns the value following `-c`, when present.
    pub fn start_directory_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-c")
    }

    /// Returns whether either supplied spelling is present.
    pub fn has_flag(&self, short: &str, long: &str) -> bool {
        self.args
            .iter()
            .any(|argument| argument == short || argument == long)
    }
}

/// Parses one or more semicolon-separated command invocations.
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
        return Err(MuxError::invalid_args(
            "command input did not contain a command",
        ));
    }
    Ok(commands)
}

fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].as_str())
}

fn positional_args_from_slice(args: &[String]) -> Vec<&str> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        if matches!(
            argument,
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
        if argument.starts_with('-') {
            index += 1;
            continue;
        }
        values.push(argument);
        index += 1;
    }
    values
}

fn split_semicolon_sequence(input: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' if quote != QuoteState::Single => {
                escaped = true;
                current.push(character);
            }
            '\'' if quote == QuoteState::None => {
                quote = QuoteState::Single;
                current.push(character);
            }
            '\'' if quote == QuoteState::Single => {
                quote = QuoteState::None;
                current.push(character);
            }
            '"' if quote == QuoteState::None => {
                quote = QuoteState::Double;
                current.push(character);
            }
            '"' if quote == QuoteState::Double => {
                quote = QuoteState::None;
                current.push(character);
            }
            ';' if quote == QuoteState::None => {
                segments.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(character),
        }
    }
    if escaped {
        return Err(MuxError::invalid_args("command sequence ends with escape"));
    }
    if quote != QuoteState::None {
        return Err(MuxError::invalid_args(
            "unterminated quoted command argument",
        ));
    }
    segments.push(current.trim().to_string());
    Ok(segments)
}

fn tokenize_command(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = QuoteState::None;
    let mut escaped = false;
    let mut token_started = false;
    for character in input.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            token_started = true;
            continue;
        }
        match character {
            '\\' if quote != QuoteState::Single => escaped = true,
            '\'' if quote == QuoteState::None => {
                quote = QuoteState::Single;
                token_started = true;
            }
            '\'' if quote == QuoteState::Single => quote = QuoteState::None,
            '"' if quote == QuoteState::None => {
                quote = QuoteState::Double;
                token_started = true;
            }
            '"' if quote == QuoteState::Double => quote = QuoteState::None,
            character if character.is_whitespace() && quote == QuoteState::None => {
                if token_started {
                    tokens.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            _ => {
                current.push(character);
                token_started = true;
            }
        }
    }
    if escaped {
        return Err(MuxError::invalid_args("command ends with escape"));
    }
    if quote != QuoteState::None {
        return Err(MuxError::invalid_args(
            "unterminated quoted command argument",
        ));
    }
    if token_started {
        tokens.push(current);
    }
    Ok(tokens)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteState {
    None,
    Single,
    Double,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies quoted arguments and target flags survive command parsing.
    #[test]
    fn parses_command_with_quotes_and_target_flag() {
        let commands = parse_command_sequence("rename-window -t @1 \"work tree\"").unwrap();

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "rename-window");
        assert_eq!(commands[0].target_arg(), Some("@1"));
        assert_eq!(commands[0].args[2], "work tree");
    }

    /// Verifies explicit empty quoted arguments remain present and ordered.
    #[test]
    fn preserves_explicit_empty_quoted_arguments() {
        let commands = parse_command_sequence("send --body \"\" '' keep").unwrap();

        assert_eq!(
            commands[0].args,
            vec!["--body", "", "", "keep"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    /// Verifies semicolons split commands only outside quoted arguments.
    #[test]
    fn splits_semicolon_sequence_outside_quotes() {
        let commands = parse_command_sequence("select-window -t @1; rename-window 'a;b'").unwrap();

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].name, "select-window");
        assert_eq!(commands[1].args[0], "a;b");
    }

    /// Verifies unterminated quoted arguments fail with the mux invalid-args
    /// category and stable diagnostic.
    #[test]
    fn rejects_unterminated_quotes() {
        let error = parse_command_sequence("rename-window \"unterminated").unwrap_err();

        assert_eq!(error.kind(), crate::MuxErrorKind::InvalidArgs);
        assert!(error.message().contains("unterminated quoted"));
    }
}
