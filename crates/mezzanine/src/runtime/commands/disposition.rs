//! Inline versus deferred execution disposition for runtime slash commands.
//!
//! Agent-prompt slash commands are submitted inside an attached-client actor
//! request, so any command that reads a store or the filesystem executes that
//! work on the serialized actor today. This module owns the classification that
//! decides which commands keep running inline (actor-owned prompt, model, and
//! policy mutation) and which move to the deferred executor, plus the guard test
//! that keeps the classification exhaustive over the registry in
//! [`mez_agent::slash::baseline_slash_commands`].
//!
//! Until the deferred executor lands, every command still executes inline; the
//! classification is the contract that executor consumes.

/// Execution lane for one runtime slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeAgentSlashCommandDisposition {
    /// The command mutates actor-owned prompt, model, or policy state and must
    /// run inside the serialized actor request that submitted it.
    Inline,
    /// The command touches stores or files and must run on the deferred
    /// executor so its I/O never parks the actor.
    Deferred,
}

/// Commands that keep running inline on the actor.
///
/// Every entry mutates state the actor owns (the agent shell session, the active
/// model, or policy) and does no store or filesystem read that could block.
#[allow(
    dead_code,
    reason = "f526838b phase 2 step (a): the deferred executor consumes this classification next"
)]
pub(crate) const RUNTIME_AGENT_INLINE_SLASH_COMMANDS: &[&str] = &[
    "help",
    "permissions",
    "approval",
    "approve",
    "sandbox",
    "shell-mode",
    "objective",
    "compact",
    "copy",
    "directive",
    "exit",
    "show-context",
    "status",
    "plan",
    "model",
    "thinking",
    "latency",
    "routing",
    "personality",
    "stop",
    "name-session",
    "reset-status",
    "log-level",
    "loop",
    "clear",
    "new",
    "remember",
];

/// Commands that move to the deferred executor.
///
/// Every entry reads a store (transcripts, issues, memory, records), the skill
/// catalog, or project files, or performs provider/network work; unknown
/// commands default here as well.
#[allow(
    dead_code,
    reason = "f526838b phase 2 step (a): the deferred executor consumes this classification next"
)]
pub(crate) const RUNTIME_AGENT_DEFERRED_SLASH_COMMANDS: &[&str] = &[
    "show-approvals",
    "list-macros",
    "list-skills",
    "sync-builtin-skills",
    "list-modified-files",
    "copy-context",
    "copy-trace-log",
    "copy-patches",
    "init",
    "auth-status",
    "refresh-provider-info",
    "list-mcp",
    "issue",
    "context-doc",
    "editor-recovery",
    "show-issues",
    "memory",
    "show-memories",
    "list-personalities",
    "fork",
    "resume",
    "debug-config",
];

/// Runtime commands that must stay inline although they are not part of the
/// dependency-neutral mez-agent slash registry.
///
/// These commands render live in-memory state: session, pane, client, message,
/// and agent bookkeeping plus, for `show-metrics`, the actor's cached metrics
/// snapshot that is published on demand for the request families that run
/// display commands. The deferred executor would classify an unknown command as
/// deferred, move a pure display off actor for no I/O benefit, and - for
/// `show-metrics` - read a snapshot that was never published for that path, so
/// they are pinned inline here instead of falling through the unknown-command
/// default. Aliases of runtime-only display commands are pinned with them
/// because the dispatcher matches the alias spelling directly.
#[allow(
    dead_code,
    reason = "f526838b phase 2 step (a): the deferred executor consumes this classification next"
)]
pub(crate) const RUNTIME_AGENT_INLINE_DISPLAY_COMMANDS: &[&str] = &[
    "list-clients",
    "listc",
    "list-panes",
    "listp",
    "show-messages",
    "show-metrics",
    "show-pane-status",
];

/// Returns the execution lane for one canonical slash command name.
///
/// Unknown names default to [`RuntimeAgentSlashCommandDisposition::Deferred`]
/// so a command this runtime does not model still keeps its I/O off the actor.
#[allow(
    dead_code,
    reason = "f526838b phase 2 step (a): the deferred executor consumes this classifier next"
)]
pub(crate) fn runtime_agent_slash_command_disposition(
    name: &str,
) -> RuntimeAgentSlashCommandDisposition {
    if RUNTIME_AGENT_INLINE_SLASH_COMMANDS.contains(&name) {
        return RuntimeAgentSlashCommandDisposition::Inline;
    }
    if RUNTIME_AGENT_INLINE_DISPLAY_COMMANDS.contains(&name) {
        return RuntimeAgentSlashCommandDisposition::Inline;
    }
    RuntimeAgentSlashCommandDisposition::Deferred
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Verifies the classification covers the stable slash-command registry
    /// exactly once per command, so a new registry entry cannot silently land in
    /// neither lane.
    #[test]
    fn runtime_slash_command_disposition_covers_the_registry() {
        let registry = mez_agent::slash::baseline_slash_commands();
        let mut classified = BTreeSet::new();
        for name in RUNTIME_AGENT_INLINE_SLASH_COMMANDS
            .iter()
            .chain(RUNTIME_AGENT_DEFERRED_SLASH_COMMANDS)
        {
            assert!(
                classified.insert(*name),
                "`{name}` is classified more than once"
            );
        }
        let aliases = registry
            .iter()
            .flat_map(|spec| spec.aliases.iter().copied())
            .collect::<BTreeSet<_>>();
        for name in &classified {
            assert!(
                !aliases.contains(*name),
                "aliases resolve through their canonical command instead of being classified: {name}"
            );
        }
        for spec in &registry {
            assert!(
                classified.contains(spec.name),
                "registry command `{}` has no execution lane",
                spec.name
            );
            let expected = if RUNTIME_AGENT_INLINE_SLASH_COMMANDS.contains(&spec.name) {
                RuntimeAgentSlashCommandDisposition::Inline
            } else {
                RuntimeAgentSlashCommandDisposition::Deferred
            };
            assert_eq!(
                runtime_agent_slash_command_disposition(spec.name),
                expected,
                "`{}` resolves to the lane its list declares",
                spec.name
            );
        }
        assert!(
            classified.len() == registry.len(),
            "classification lists must not keep names the registry dropped: {classified:?}"
        );
        for name in RUNTIME_AGENT_INLINE_DISPLAY_COMMANDS {
            assert!(
                !classified.contains(*name),
                "display commands live outside the registry and stay in their own list: {name}"
            );
        }
    }

    /// Verifies in-memory display commands stay inline even though they are not
    /// registry commands, and that the registry `status` command shares the lane
    /// because its display never touches a store.
    #[test]
    fn runtime_slash_command_disposition_keeps_display_commands_inline() {
        for name in RUNTIME_AGENT_INLINE_DISPLAY_COMMANDS {
            assert_eq!(
                runtime_agent_slash_command_disposition(name),
                RuntimeAgentSlashCommandDisposition::Inline,
                "{name} renders live in-memory state and must stay inline"
            );
        }
        assert_eq!(
            runtime_agent_slash_command_disposition("status"),
            RuntimeAgentSlashCommandDisposition::Inline,
            "status reads only in-memory session and provider bookkeeping"
        );
    }

    /// Verifies unknown commands default to the deferred lane.
    #[test]
    fn runtime_slash_command_disposition_defers_unknown_commands() {
        assert_eq!(
            runtime_agent_slash_command_disposition("not-a-runtime-command"),
            RuntimeAgentSlashCommandDisposition::Deferred
        );
    }
}
