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
//! Static lane, off-actor membership and awaited host effects are independent
//! axes. Live store, origin and argument eligibility remain with their callers.

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

/// Host effect that the asynchronous agent-shell executor must await.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentShellAwaitedCommand {
    /// Pane model or routing-model selection.
    Model,
    /// Model-backed conversation compaction queueing.
    Compact,
    /// Model-backed durable-memory extraction.
    Remember,
    /// MCP listing after live transport discovery.
    ListMcp,
    /// Provider catalog refresh through the async runtime.
    RefreshProviderInfo,
}

/// Static runtime-only execution properties, independent of live eligibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CommandExecutionMetadata {
    /// Canonical registry command or runtime-only display spelling.
    pub name: &'static str,
    /// Actor classification; this alone never authorizes worker admission.
    pub disposition: RuntimeAgentSlashCommandDisposition,
    /// Whether the command has an off-actor prepared-input executor.
    pub off_actor: bool,
    /// Optional awaited effect, separate from the off-actor lane.
    pub awaited: Option<AgentShellAwaitedCommand>,
    /// Whether this spelling is a runtime-only display command or alias.
    pub runtime_display: bool,
}

impl CommandExecutionMetadata {
    const fn inline(name: &'static str, awaited: Option<AgentShellAwaitedCommand>) -> Self {
        Self {
            name,
            disposition: RuntimeAgentSlashCommandDisposition::Inline,
            off_actor: false,
            awaited,
            runtime_display: false,
        }
    }

    const fn deferred(
        name: &'static str,
        off_actor: bool,
        awaited: Option<AgentShellAwaitedCommand>,
    ) -> Self {
        Self {
            name,
            disposition: RuntimeAgentSlashCommandDisposition::Deferred,
            off_actor,
            awaited,
            runtime_display: false,
        }
    }

    const fn display(name: &'static str) -> Self {
        Self {
            name,
            disposition: RuntimeAgentSlashCommandDisposition::Inline,
            off_actor: false,
            awaited: None,
            runtime_display: true,
        }
    }
}

use AgentShellAwaitedCommand as Awaited;
use CommandExecutionMetadata as Entry;

/// Product-owned metadata for every canonical slash command plus live display
/// spellings. Runtime-only display aliases are explicit because their dispatcher
/// matches the spelling directly; registry aliases resolve to canonical names.
pub(super) const COMMAND_EXECUTION_METADATA: &[CommandExecutionMetadata] = &[
    Entry::inline("help", None),
    Entry::inline("permissions", None),
    Entry::inline("approval", None),
    Entry::inline("approve", None),
    Entry::inline("sandbox", None),
    Entry::inline("shell-mode", None),
    Entry::inline("objective", None),
    Entry::inline("compact", Some(Awaited::Compact)),
    Entry::inline("copy", None),
    Entry::inline("directive", None),
    Entry::inline("exit", None),
    Entry::inline("status", None),
    Entry::inline("plan", None),
    Entry::inline("model", Some(Awaited::Model)),
    Entry::inline("thinking", None),
    Entry::inline("latency", None),
    Entry::inline("routing", None),
    Entry::inline("personality", None),
    Entry::inline("stop", None),
    Entry::inline("name-session", None),
    Entry::inline("reset-status", None),
    Entry::inline("log-level", None),
    Entry::inline("loop", None),
    Entry::inline("clear", None),
    Entry::inline("new", None),
    Entry::inline("remember", Some(Awaited::Remember)),
    Entry::deferred("show-approvals", true, None),
    Entry::deferred("list-macros", true, None),
    Entry::deferred("list-skills", true, None),
    Entry::deferred("sync-builtin-skills", true, None),
    Entry::deferred("list-modified-files", true, None),
    Entry::deferred("copy-context", false, None),
    Entry::deferred("copy-trace-log", false, None),
    Entry::deferred("copy-patches", false, None),
    Entry::deferred("init", false, None),
    Entry::deferred("auth-status", true, None),
    Entry::deferred(
        "refresh-provider-info",
        false,
        Some(Awaited::RefreshProviderInfo),
    ),
    Entry::deferred("list-mcp", false, Some(Awaited::ListMcp)),
    Entry::deferred("issue", true, None),
    Entry::deferred("context-doc", true, None),
    Entry::deferred("editor-recovery", false, None),
    Entry::deferred("show-issues", true, None),
    Entry::deferred("memory", false, None),
    Entry::deferred("show-memories", true, None),
    Entry::deferred("show-context", true, None),
    Entry::deferred("list-personalities", true, None),
    Entry::deferred("fork", false, None),
    Entry::deferred("resume", true, None),
    Entry::deferred("debug-config", false, None),
    Entry::display("list-clients"),
    Entry::display("listc"),
    Entry::display("list-panes"),
    Entry::display("listp"),
    Entry::display("show-messages"),
    Entry::display("show-metrics"),
    Entry::display("show-pane-status"),
];

/// Looks up a known canonical command or runtime-only display spelling.
pub(super) fn command_execution_metadata(name: &str) -> Option<&'static CommandExecutionMetadata> {
    COMMAND_EXECUTION_METADATA
        .iter()
        .find(|entry| entry.name == name)
}

/// Returns the execution lane for one canonical slash command name.
///
/// Unknown names default to [`RuntimeAgentSlashCommandDisposition::Deferred`]
/// so a command this runtime does not model still keeps its I/O off the actor.
pub(crate) fn runtime_agent_slash_command_disposition(
    name: &str,
) -> RuntimeAgentSlashCommandDisposition {
    command_execution_metadata(name)
        .map_or(RuntimeAgentSlashCommandDisposition::Deferred, |entry| {
            entry.disposition
        })
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
        for entry in COMMAND_EXECUTION_METADATA
            .iter()
            .filter(|entry| !entry.runtime_display)
        {
            let name = entry.name;
            assert!(
                classified.insert(name),
                "`{name}` is classified more than once"
            );
        }
        let aliases = registry
            .iter()
            .flat_map(|spec| spec.aliases.iter().copied())
            .collect::<BTreeSet<_>>();
        for name in &classified {
            assert!(
                !aliases.contains(name),
                "aliases resolve through their canonical command instead of being classified: {name}"
            );
        }
        for spec in &registry {
            assert!(
                classified.contains(spec.name),
                "registry command `{}` has no execution lane",
                spec.name
            );
            let entry = command_execution_metadata(spec.name).unwrap();
            assert!(!entry.runtime_display);
            assert_eq!(
                runtime_agent_slash_command_disposition(spec.name),
                entry.disposition,
                "`{}` resolves to the lane its metadata declares",
                spec.name
            );
        }
        assert!(
            classified.len() == registry.len(),
            "metadata must not keep names the registry dropped: {classified:?}"
        );
        for name in COMMAND_EXECUTION_METADATA
            .iter()
            .filter(|entry| entry.runtime_display)
            .map(|entry| entry.name)
        {
            assert!(
                !classified.contains(name),
                "display commands live outside the registry: {name}"
            );
        }
    }

    /// Verifies in-memory display commands stay inline even though they are not
    /// registry commands, and that the registry `status` command shares the lane
    /// because its display never touches a store.
    #[test]
    fn runtime_slash_command_disposition_keeps_display_commands_inline() {
        for name in [
            "list-clients",
            "listc",
            "list-panes",
            "listp",
            "show-messages",
            "show-metrics",
            "show-pane-status",
        ] {
            let entry = command_execution_metadata(name).unwrap();
            assert!(entry.runtime_display);
            assert!(!entry.off_actor);
            assert_eq!(entry.awaited, None);
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
        assert!(command_execution_metadata("not-a-runtime-command").is_none());
    }

    /// Pins independent off-actor and awaited axes so an actor classification
    /// cannot accidentally admit an unprepared worker or lose an async effect.
    #[test]
    fn runtime_command_metadata_preserves_execution_axes() {
        let off_actor = COMMAND_EXECUTION_METADATA
            .iter()
            .filter(|entry| entry.off_actor)
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            off_actor,
            BTreeSet::from([
                "list-skills",
                "list-macros",
                "auth-status",
                "issue",
                "show-issues",
                "show-memories",
                "show-context",
                "context-doc",
                "sync-builtin-skills",
                "resume",
                "list-modified-files",
                "show-approvals",
                "list-personalities",
            ])
        );
        let awaited = COMMAND_EXECUTION_METADATA
            .iter()
            .filter_map(|entry| entry.awaited.map(|effect| (entry.name, effect)))
            .collect::<Vec<_>>();
        assert_eq!(
            awaited,
            [
                ("compact", Awaited::Compact),
                ("model", Awaited::Model),
                ("remember", Awaited::Remember),
                ("refresh-provider-info", Awaited::RefreshProviderInfo),
                ("list-mcp", Awaited::ListMcp),
            ]
        );
        assert_eq!(
            command_execution_metadata("copy-context")
                .unwrap()
                .disposition,
            RuntimeAgentSlashCommandDisposition::Deferred
        );
        assert!(
            !command_execution_metadata("copy-context")
                .unwrap()
                .off_actor
        );
    }
}
