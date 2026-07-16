//! Runtime agent macro discovery and managed-step helpers.
//!
//! This module keeps pane-scoped macro catalog discovery beside the skill
//! discovery helpers. It also owns the narrow bridge that lets macro-managed
//! step traffic become ordinary agent-shell turns in a persistent child
//! subagent session.
use crate::macros::{discover_macro_catalog, load_macro_definition};
use crate::project::TrustDecision;
use crate::runtime::agent_state::RuntimeAgentLoopCompletion;
use crate::runtime::service_state::{
    MacroJudgeDecision, MacroJudgeOutcome, MacroManagedSubagent, MacroRunPhase, MacroRunState,
    MacroRunStep,
};
use crate::runtime::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, RuntimeAgentPromptTurnStart,
    execute_agent_shell_command_with_context,
};
use mez_agent::{
    AllowedActionSet, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest,
};
use mez_agent::{MacroCatalog, MacroDefinition, parse_macro_prompt_invocation};
use mez_agent::{ScheduledWork, ScheduledWorkKind};

mod helpers;
mod judge;
mod lifecycle;
mod message;

#[cfg(test)]
mod tests;
