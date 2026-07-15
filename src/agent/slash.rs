//! Agent Slash implementation.
//!
//! This module owns the agent slash boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentLogLevel, AgentShellStore, AgentShellVisibility, MezError, Result,
    agent_shell_help_display, agent_shell_mcp_display, agent_shell_permissions_display,
    agent_shell_status_display,
};
use mez_agent::{
    AgentShellMcpSummary, AgentShellPermissionSummary, AgentShellSessionError,
    AgentShellSessionErrorKind, AgentShellSessionResult, SlashCommandInvocation,
    parse_slash_command as parse_agent_slash_command,
};

// Agent shell slash command registry and dispatch.

/// Carries Agent Shell Command Outcome state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentShellCommandOutcome {
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
        /// Stores the body value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        body: String,
        /// Stores the visibility value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        visibility: AgentShellVisibility,
    },
    /// Represents the Requires Runtime case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    RequiresRuntime {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the reason value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reason: String,
    },
}

/// Runs the parse slash command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_slash_command(input: &str) -> Result<Option<SlashCommandInvocation>> {
    parse_agent_slash_command(input).map_err(|error| MezError::invalid_args(error.to_string()))
}

/// Runs the execute agent shell command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_agent_shell_command(
    store: &mut AgentShellStore,
    pane_id: &str,
    input: &str,
) -> Result<Option<AgentShellCommandOutcome>> {
    execute_agent_shell_command_with_runtime_context(store, pane_id, input, None)
}

/// Runs the execute agent shell command with mcp operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_agent_shell_command_with_mcp(
    store: &mut AgentShellStore,
    pane_id: &str,
    input: &str,
    mcp_summary: Option<&AgentShellMcpSummary>,
) -> Result<Option<AgentShellCommandOutcome>> {
    execute_agent_shell_command_with_runtime_context(store, pane_id, input, mcp_summary)
}

/// Runs the execute agent shell command with runtime context operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_agent_shell_command_with_runtime_context(
    store: &mut AgentShellStore,
    pane_id: &str,
    input: &str,
    mcp_summary: Option<&AgentShellMcpSummary>,
) -> Result<Option<AgentShellCommandOutcome>> {
    execute_agent_shell_command_with_context(
        store,
        pane_id,
        input,
        AgentShellRuntimeContext {
            mcp_summary,
            permission_summary: None,
        },
    )
}

/// Runs the execute agent shell command with permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_agent_shell_command_with_permissions(
    store: &mut AgentShellStore,
    pane_id: &str,
    input: &str,
    permission_summary: &AgentShellPermissionSummary,
) -> Result<Option<AgentShellCommandOutcome>> {
    execute_agent_shell_command_with_context(
        store,
        pane_id,
        input,
        AgentShellRuntimeContext {
            mcp_summary: None,
            permission_summary: Some(permission_summary),
        },
    )
}

/// Carries Agent Shell Runtime Context state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, Default)]
pub struct AgentShellRuntimeContext<'a> {
    /// Stores the mcp registry value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mcp_summary: Option<&'a AgentShellMcpSummary>,
    /// Stores the permission policy value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub permission_summary: Option<&'a AgentShellPermissionSummary>,
}

/// Runs the execute agent shell command with context operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_agent_shell_command_with_context(
    store: &mut AgentShellStore,
    pane_id: &str,
    input: &str,
    context: AgentShellRuntimeContext<'_>,
) -> Result<Option<AgentShellCommandOutcome>> {
    match execute_agent_shell_command_with_context_inner(store, pane_id, input, context) {
        Ok(outcome) => Ok(outcome),
        Err(error)
            if input.trim_start().starts_with('/')
                && matches!(
                    error.kind(),
                    AgentShellSessionErrorKind::InvalidArgs
                        | AgentShellSessionErrorKind::Conflict
                        | AgentShellSessionErrorKind::NotFound
                ) =>
        {
            Ok(Some(agent_shell_command_error_outcome(input, &error)))
        }
        Err(error) => Err(error.into()),
    }
}

/// Runs the execute agent shell command with context inner operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn execute_agent_shell_command_with_context_inner(
    store: &mut AgentShellStore,
    pane_id: &str,
    input: &str,
    context: AgentShellRuntimeContext<'_>,
) -> AgentShellSessionResult<Option<AgentShellCommandOutcome>> {
    let Some(invocation) = parse_agent_slash_command(input)
        .map_err(|error| AgentShellSessionError::invalid_args(error.to_string()))?
    else {
        return Ok(None);
    };
    if !invocation.queueable_while_running
        && store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
    {
        return Err(AgentShellSessionError::conflict(format!(
            "/{} cannot run while an agent turn is active in this pane",
            invocation.name
        )));
    }
    let command = invocation.name.clone();
    let outcome = match invocation.name.as_str() {
        "help" => AgentShellCommandOutcome::Display {
            command,
            body: agent_shell_help_display(),
        },
        "status" => {
            let session = store.get(pane_id).ok_or_else(|| {
                AgentShellSessionError::not_found("agent shell session not found for pane")
            })?;
            AgentShellCommandOutcome::Display {
                command,
                body: agent_shell_status_display(session),
            }
        }
        "list-sessions" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "session listing must be written through the live pane runtime".to_string(),
        },
        "list-macros" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "macro listing requires live runtime macro discovery".to_string(),
        },
        "list-skills" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "skill listing requires live runtime skill discovery".to_string(),
        },
        "sync-builtin-skills" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "managed built-in skill synchronization requires the live user config root"
                .to_string(),
        },
        "list-modified-files" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "modified-file listing requires the live pane runtime".to_string(),
        },
        "copy-context" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "model request context dumps require the live running turn".to_string(),
        },
        "copy-trace-log" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "pane trace log exports require the live runtime".to_string(),
        },
        "copy-patches" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "patch exports require the live runtime".to_string(),
        },
        "directive" => {
            let requested = invocation.args.trim();
            let session = if requested.is_empty() || matches!(requested, "status" | "show") {
                store.get(pane_id).ok_or_else(|| {
                    AgentShellSessionError::not_found("agent shell session not found for pane")
                })?
            } else if matches!(requested, "clear" | "default" | "none") {
                store.set_directive(pane_id, None)?
            } else {
                store.set_directive(pane_id, Some(requested.to_string()))?
            };
            if requested.is_empty() || matches!(requested, "status" | "show") {
                AgentShellCommandOutcome::Display {
                    command,
                    body: format!(
                        "agent directive for pane {} is {}.",
                        session.pane_id,
                        session
                            .directive
                            .as_deref()
                            .map(|directive| format!("`{}`", directive.replace('`', "\\`")))
                            .unwrap_or_else(|| "not set".to_string())
                    ),
                }
            } else {
                AgentShellCommandOutcome::Mutated {
                    command,
                    body: format!(
                        "agent directive for pane {} is now {}.",
                        session.pane_id,
                        session
                            .directive
                            .as_deref()
                            .map(|directive| format!("`{}`", directive.replace('`', "\\`")))
                            .unwrap_or_else(|| "not set".to_string())
                    ),
                    visibility: session.visibility,
                }
            }
        }
        "permissions" => match context.permission_summary {
            Some(summary) if invocation.args.is_empty() => AgentShellCommandOutcome::Display {
                command,
                body: agent_shell_permissions_display(*summary),
            },
            Some(_) => AgentShellCommandOutcome::RequiresRuntime {
                command,
                reason:
                    "permission changes require primary-client approval through the live runtime"
                        .to_string(),
            },
            None => AgentShellCommandOutcome::RequiresRuntime {
                command,
                reason: "permission inspection requires the live permission policy".to_string(),
            },
        },
        "approval" => match context.permission_summary {
            Some(summary) if invocation.args.is_empty() => AgentShellCommandOutcome::Display {
                command,
                body: format!(
                    "approval_policy={} source=runtime-policy",
                    summary.approval_policy.as_str()
                ),
            },
            Some(_) => AgentShellCommandOutcome::RequiresRuntime {
                command,
                reason: "approval mode changes require the live runtime".to_string(),
            },
            None => AgentShellCommandOutcome::RequiresRuntime {
                command,
                reason: "approval mode inspection requires the live permission policy".to_string(),
            },
        },
        "list-mcp" => match context.mcp_summary {
            Some(summary) => AgentShellCommandOutcome::Display {
                command,
                body: agent_shell_mcp_display(summary),
            },
            None => AgentShellCommandOutcome::RequiresRuntime {
                command,
                reason: "MCP listing requires the live MCP registry".to_string(),
            },
        },
        "memory" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "persistent memory enablement changes require the live runtime".to_string(),
        },
        "issue" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "local issue tracking requires the live runtime".to_string(),
        },
        "show-issues" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "issue browser display requires the live runtime".to_string(),
        },
        "show-memories" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "memory browser display requires the live runtime".to_string(),
        },
        "remember" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "durable memory generation requires the live model runtime".to_string(),
        },
        "latency" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "latency preference changes require the live runtime".to_string(),
        },
        "thinking" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "provider thinking mode changes require the live runtime".to_string(),
        },
        "clear" => {
            let session = store.start_new_conversation(pane_id)?;
            AgentShellCommandOutcome::Mutated {
                command,
                body: format!(
                    "pane={} session={} transcript_entries=0 new=true",
                    session.pane_id, session.session_id
                ),
                visibility: session.visibility,
            }
        }
        "exit" => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: "agent shell exit requires the live runtime stop/visibility boundary"
                .to_string(),
        },
        "new" => {
            let session = store.start_new_conversation(pane_id)?;
            AgentShellCommandOutcome::Mutated {
                command,
                body: format!(
                    "pane={} session={} transcript_entries=0 new=true",
                    session.pane_id, session.session_id
                ),
                visibility: session.visibility,
            }
        }
        "log-level" => {
            let requested = invocation.args.trim();
            if requested.is_empty() {
                let session = store.get(pane_id).ok_or_else(|| {
                    AgentShellSessionError::not_found("agent shell session not found for pane")
                })?;
                return Ok(Some(AgentShellCommandOutcome::Display {
                    command,
                    body: format!(
                        "agent log level for pane {} is {}.\navailable levels: normal, verbose, debug, trace.",
                        session.pane_id,
                        session.log_level.as_str()
                    ),
                }));
            }
            let mut args = requested.split_whitespace();
            let level_name = args.next().unwrap_or_default();
            if args.next().is_some() {
                return Err(AgentShellSessionError::invalid_args(
                    "log-level expects one of: normal, verbose, debug, trace",
                ));
            }
            let level = AgentLogLevel::parse(level_name).ok_or_else(|| {
                AgentShellSessionError::invalid_args(
                    "log-level expects one of: normal, verbose, debug, trace",
                )
            })?;
            let session = store.set_log_level(pane_id, level)?;
            AgentShellCommandOutcome::Mutated {
                command,
                body: format!(
                    "agent log level for pane {} is now {}.",
                    session.pane_id,
                    session.log_level.as_str()
                ),
                visibility: session.visibility,
            }
        }
        _ => AgentShellCommandOutcome::RequiresRuntime {
            command,
            reason: format!(
                "slash command effect {:?} requires the live agent runtime",
                invocation.effect
            ),
        },
    };
    Ok(Some(outcome))
}

/// Runs the agent shell command error outcome operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_shell_command_error_outcome(
    input: &str,
    error: &AgentShellSessionError,
) -> AgentShellCommandOutcome {
    let command = input
        .split_whitespace()
        .next()
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_string();
    AgentShellCommandOutcome::Display {
        command,
        body: format!("agent command error: {}", error.message()),
    }
}
