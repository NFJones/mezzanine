//! Command Types implementation.
//!
//! This module owns the command types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::flag_value;

// Command invocation, outcome, and baseline registry types.

/// Carries Command Invocation state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInvocation {
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the args value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub args: Vec<String>,
}

impl CommandInvocation {
    /// Runs the target arg operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn target_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-t")
    }

    /// Runs the source arg operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn source_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-s")
    }

    /// Runs the start directory arg operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_directory_arg(&self) -> Option<&str> {
        flag_value(&self.args, "-c")
    }

    /// Runs the has flag operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn has_flag(&self, short: &str, long: &str) -> bool {
        self.args
            .iter()
            .any(|arg| arg.as_str() == short || arg.as_str() == long)
    }
}

/// Carries Command Outcome state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Represents the Noop case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Noop {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
    },
    /// Represents the Mutated case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Mutated {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
    },
    /// Represents the Mutated With Pane Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MutatedWithPaneCommand {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the shell command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        shell_command: String,
        /// Stores the start directory value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        start_directory: Option<String>,
    },
    /// Represents the Display case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Display {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the body value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        body: String,
    },
}

/// Baseline command support level exposed by `list-commands`.
///
/// This describes where the command's authoritative behavior is available.
/// Commands with a fallback still parse and return safe diagnostics from the
/// generic command dispatcher, but their full behavior requires the named
/// backing context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaselineCommandStatus {
    /// The generic in-memory/session command dispatcher implements the command.
    Implemented,
    /// Full behavior requires an attached terminal runtime.
    RuntimeRequired,
    /// Full behavior requires a runtime or explicit persistent store context.
    StoreRequired,
    /// Full behavior is exposed through a control/repository path, not the
    /// generic command fallback.
    ControlRequired,
}

impl BaselineCommandStatus {
    /// Returns the stable status label used by command displays and tests.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::RuntimeRequired => "runtime-required",
            Self::StoreRequired => "store-required",
            Self::ControlRequired => "control-required",
        }
    }
}

/// Carries Baseline Command state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineCommand {
    /// Canonical command name from the baseline command registry.
    pub name: &'static str,
    /// Current support level for authoritative command behavior.
    pub status: BaselineCommandStatus,
}

/// Defines the BASELINE COMMAND NAMES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(super) const BASELINE_COMMAND_NAMES: &[&str] = &[
    "help",
    "new-window",
    "rename-window",
    "kill-window",
    "select-window",
    "next-window",
    "previous-window",
    "last-window",
    "new-group",
    "rename-group",
    "kill-group",
    "select-group",
    "next-group",
    "previous-group",
    "last-group",
    "split-window",
    "kill-pane",
    "select-pane",
    "resize-pane",
    "next-pane",
    "previous-pane",
    "last-pane",
    "rotate-pane",
    "next-layout",
    "select-layout",
    "rebalance-window",
    "zoom-pane",
    "swap-pane",
    "break-pane",
    "join-pane",
    "display-panes",
    "list-groups",
    "choose-group",
    "list-windows",
    "list-panes",
    "list-clients",
    "detach-client",
    "attach-session",
    "list-sessions",
    "rename-session",
    "kill-session",
    "copy-mode",
    "copy-selection",
    "paste-clipboard",
    "paste-buffer",
    "create-buffer",
    "list-buffers",
    "choose-buffer",
    "delete-buffer",
    "show-messages",
    "list-keys",
    "list-themes",
    "set-theme",
    "bind-key",
    "unbind-key",
    "show-options",
    "set-option",
    "source-file",
    "refresh-client",
    "refresh-provider-info",
    "agent-shell",
    "auth-login",
    "auth-status",
    "mcp-add",
    "mcp-remove",
    "mcp-retry",
    "snapshot-session",
    "resume-session",
    "capture-pane",
    "save-buffer",
    "clear-history",
    "search-history",
    "export-history",
    "pipe-pane",
    "mark-pane-ready",
    "list-observers",
    "choose-observer",
    "approve-observer",
    "reject-observer",
    "revoke-observer",
];

/// Runs the baseline command status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn baseline_command_status(name: &str) -> BaselineCommandStatus {
    match name {
        "copy-mode"
        | "copy-selection"
        | "paste-clipboard"
        | "paste-buffer"
        | "create-buffer"
        | "list-buffers"
        | "choose-buffer"
        | "delete-buffer"
        | "capture-pane"
        | "save-buffer"
        | "clear-history"
        | "search-history"
        | "export-history"
        | "pipe-pane"
        | "refresh-client"
        | "refresh-provider-info"
        | "agent-shell"
        | "mcp-retry"
        | "approve-observer"
        | "reject-observer"
        | "revoke-observer" => BaselineCommandStatus::RuntimeRequired,
        "bind-key" | "unbind-key" | "set-theme" | "set-option" | "source-file" | "auth-login"
        | "auth-status" | "mcp-add" | "mcp-remove" | "mark-pane-ready" => {
            BaselineCommandStatus::StoreRequired
        }
        "snapshot-session" | "resume-session" => BaselineCommandStatus::ControlRequired,
        _ => BaselineCommandStatus::Implemented,
    }
}

/// Returns the baseline command registry with support status for each command.
pub fn baseline_commands() -> Vec<BaselineCommand> {
    BASELINE_COMMAND_NAMES
        .iter()
        .map(|name| BaselineCommand {
            name,
            status: baseline_command_status(name),
        })
        .collect()
}
