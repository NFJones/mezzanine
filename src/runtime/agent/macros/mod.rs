//! Runtime agent macro discovery and managed-step helpers.
//!
//! This module keeps pane-scoped macro catalog discovery beside the skill
//! discovery helpers. It also owns the narrow bridge that lets macro-managed
//! step traffic become ordinary agent-shell turns in a persistent child
//! subagent session.
use crate::integrations::macros::{discover_macro_catalog, load_macro_definition};
use crate::runtime::agent_state::RuntimeAgentLoopCompletion;
use crate::runtime::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, RuntimeAgentPromptTurnStart,
    execute_agent_shell_command_with_context,
};
use crate::security::project::TrustDecision;
use mez_agent::ScheduledWorkKind;
use mez_agent::{
    MacroCatalog, MacroDefinition, MacroJudgeDecision, MacroJudgeOutcome, MacroRunPhase,
    MacroRunRegistration, ModelRequest, macro_initial_step_prompt, macro_judge_decision_from_text,
    macro_judge_model_request, macro_message_recipient_agent_id, macro_parent_orchestration_prompt,
    macro_run_state, macro_step_model_request, parse_macro_prompt_invocation,
};

mod judge;
mod lifecycle;
mod message;

#[cfg(test)]
mod tests;
