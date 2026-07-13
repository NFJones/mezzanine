//! Dependency-neutral agent slash-command contracts and parsing.
//!
//! This module owns the stable slash-command registry, aliases, effect
//! classification, and invocation parsing. Product command execution,
//! presentation, persistence, and runtime mutation remain in Mezzanine.

use std::fmt;

/// Describes the externally visible effect class of an agent slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandEffect {
    /// The command only reads state.
    ReadOnly,
    /// The command changes permission or model policy.
    PolicyMutation,
    /// The command changes stored credentials.
    CredentialMutation,
    /// The command changes the active agent session.
    SessionMutation,
    /// The command changes project or user files.
    FileMutation,
    /// The command changes a background job.
    BackgroundJobMutation,
}

/// Stable registry metadata for one agent slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandSpec {
    /// Canonical command name without the leading slash.
    pub name: &'static str,
    /// Accepted aliases without the leading slash.
    pub aliases: &'static [&'static str],
    /// Effect classification used by runtime policy and presentation.
    pub effect: SlashCommandEffect,
    /// Whether the command may be queued while an agent turn is running.
    pub queueable_while_running: bool,
}

/// Parsed canonical invocation of one agent slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommandInvocation {
    /// Canonical command name without the leading slash.
    pub name: String,
    /// Trimmed command arguments.
    pub args: String,
    /// Effect classification copied from the registry.
    pub effect: SlashCommandEffect,
    /// Whether the command may be queued while an agent turn is running.
    pub queueable_while_running: bool,
}

/// Failure returned when slash-command text cannot be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlashCommandParseError {
    /// A slash prefix was present without a command name.
    EmptyCommand,
    /// The command name is not present in the stable registry.
    UnknownCommand,
}

impl fmt::Display for SlashCommandParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EmptyCommand => "slash command must not be empty",
            Self::UnknownCommand => "unknown slash command",
        })
    }
}

/// Returns the stable baseline agent slash-command registry.
pub fn baseline_slash_commands() -> Vec<SlashCommandSpec> {
    vec![
        slash("help", &[], SlashCommandEffect::ReadOnly, true),
        slash(
            "permissions",
            &["approvals"],
            SlashCommandEffect::PolicyMutation,
            true,
        ),
        slash("approval", &[], SlashCommandEffect::PolicyMutation, true),
        slash("approve", &[], SlashCommandEffect::PolicyMutation, true),
        slash("trust", &[], SlashCommandEffect::PolicyMutation, true),
        slash("list-sessions", &[], SlashCommandEffect::ReadOnly, true),
        slash("list-macros", &[], SlashCommandEffect::ReadOnly, true),
        slash("list-skills", &[], SlashCommandEffect::ReadOnly, true),
        slash(
            "sync-builtin-skills",
            &[],
            SlashCommandEffect::FileMutation,
            true,
        ),
        slash(
            "list-modified-files",
            &[],
            SlashCommandEffect::ReadOnly,
            true,
        ),
        slash(
            "copy-context",
            &["dump-agent-context", "dump-context"],
            SlashCommandEffect::SessionMutation,
            true,
        ),
        slash(
            "copy-trace-log",
            &[],
            SlashCommandEffect::SessionMutation,
            true,
        ),
        slash(
            "copy-patches",
            &[],
            SlashCommandEffect::SessionMutation,
            true,
        ),
        slash("clear", &[], SlashCommandEffect::SessionMutation, false),
        slash("compact", &[], SlashCommandEffect::SessionMutation, false),
        slash("copy", &[], SlashCommandEffect::SessionMutation, true),
        slash("diff", &[], SlashCommandEffect::ReadOnly, true),
        slash("directive", &[], SlashCommandEffect::SessionMutation, true),
        slash("exit", &["quit"], SlashCommandEffect::SessionMutation, true),
        slash("init", &[], SlashCommandEffect::FileMutation, true),
        slash("logout", &[], SlashCommandEffect::CredentialMutation, true),
        slash("list-mcp", &[], SlashCommandEffect::ReadOnly, true),
        slash("issue", &[], SlashCommandEffect::SessionMutation, true),
        slash("show-issues", &[], SlashCommandEffect::ReadOnly, true),
        slash("memory", &[], SlashCommandEffect::PolicyMutation, true),
        slash("show-memories", &[], SlashCommandEffect::ReadOnly, true),
        slash("remember", &[], SlashCommandEffect::SessionMutation, false),
        slash("model", &[], SlashCommandEffect::PolicyMutation, true),
        slash("thinking", &[], SlashCommandEffect::PolicyMutation, true),
        slash("latency", &[], SlashCommandEffect::PolicyMutation, true),
        slash("routing", &[], SlashCommandEffect::PolicyMutation, true),
        slash("personality", &[], SlashCommandEffect::PolicyMutation, true),
        slash("loop", &[], SlashCommandEffect::SessionMutation, false),
        slash("stop", &[], SlashCommandEffect::BackgroundJobMutation, true),
        slash("fork", &[], SlashCommandEffect::SessionMutation, false),
        slash("resume", &[], SlashCommandEffect::SessionMutation, false),
        slash("new", &[], SlashCommandEffect::SessionMutation, false),
        slash("status", &[], SlashCommandEffect::ReadOnly, true),
        slash("debug-config", &[], SlashCommandEffect::ReadOnly, true),
        slash("statusline", &[], SlashCommandEffect::SessionMutation, true),
        slash("title", &[], SlashCommandEffect::SessionMutation, true),
        slash("log-level", &[], SlashCommandEffect::SessionMutation, true),
    ]
}

/// Parses slash-command text into a canonical dependency-neutral invocation.
pub fn parse_slash_command(
    input: &str,
) -> Result<Option<SlashCommandInvocation>, SlashCommandParseError> {
    let trimmed = input.trim();
    let Some(stripped) = trimmed.strip_prefix('/') else {
        return Ok(None);
    };
    let (name, args) = if let Some(index) = stripped.find(char::is_whitespace) {
        (&stripped[..index], stripped[index..].trim())
    } else {
        (stripped, "")
    };
    if name.is_empty() {
        return Err(SlashCommandParseError::EmptyCommand);
    }
    let specs = baseline_slash_commands();
    let Some(spec) = specs
        .iter()
        .find(|spec| spec.name == name || spec.aliases.contains(&name))
    else {
        return Err(SlashCommandParseError::UnknownCommand);
    };
    Ok(Some(SlashCommandInvocation {
        name: spec.name.to_string(),
        args: args.to_string(),
        effect: spec.effect,
        queueable_while_running: spec.queueable_while_running,
    }))
}

const fn slash(
    name: &'static str,
    aliases: &'static [&'static str],
    effect: SlashCommandEffect,
    queueable_while_running: bool,
) -> SlashCommandSpec {
    SlashCommandSpec {
        name,
        aliases,
        effect,
        queueable_while_running,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SlashCommandEffect, SlashCommandParseError, baseline_slash_commands, parse_slash_command,
    };

    #[test]
    /// Verifies aliases normalize to canonical commands while preserving
    /// arguments and effect metadata used by product execution adapters.
    fn slash_parser_normalizes_aliases_and_effects() {
        let invocation = parse_slash_command(" /approvals add git status ")
            .unwrap()
            .unwrap();

        assert_eq!(invocation.name, "permissions");
        assert_eq!(invocation.args, "add git status");
        assert_eq!(invocation.effect, SlashCommandEffect::PolicyMutation);
        assert!(invocation.queueable_while_running);
    }

    #[test]
    /// Verifies ordinary prompts bypass slash parsing and malformed or unknown
    /// slash commands fail through stable typed errors.
    fn slash_parser_rejects_invalid_commands() {
        assert_eq!(parse_slash_command("ordinary prompt").unwrap(), None);
        assert_eq!(
            parse_slash_command("/").unwrap_err(),
            SlashCommandParseError::EmptyCommand
        );
        assert_eq!(
            parse_slash_command("/does-not-exist").unwrap_err(),
            SlashCommandParseError::UnknownCommand
        );
        assert!(
            baseline_slash_commands()
                .iter()
                .any(|spec| spec.name == "copy-context")
        );
    }
}
