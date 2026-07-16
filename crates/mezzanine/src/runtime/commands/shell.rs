//! Agent-shell command entry points and shell lifecycle helpers.
//!
//! This module owns the live agent-shell command dispatch path plus the pane
//! lifecycle helpers that enter, exit, and persist pane-local agent prompt
//! state. Keeping this orchestration outside the command facade leaves
//! `commands::mod` focused on cross-family command wiring while sibling
//! modules own concrete slash-command behavior.

use super::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, AgentShellVisibility, EventKind, MezError,
    Result, RuntimeSessionService, RuntimeSideEffect, agent_shell_visibility_json_name,
    agent_subshell_enter_command, execute_agent_shell_command_with_context, json_escape,
    parse_slash_command, runtime_agent_shell_command_response_json,
    runtime_agent_shell_prompt_turn_response_json, runtime_agent_shell_stop_response_json,
    runtime_mezzanine_error_code,
};
use crate::{error::MezErrorKind, runtime::commands::issues};
use mez_agent::parse_macro_prompt_invocation;

/// Result of applying the live side effects for an agent-shell exit request.
pub(crate) struct RuntimeAgentShellExit {
    /// Conversation id associated with the pane-local agent shell.
    conversation_id: String,
    /// Visibility after the exit request and any required stop operation.
    visibility: AgentShellVisibility,
    /// Turn id stopped before hiding, when exit interrupted active work.
    stopped_turn_id: Option<String>,
}

/// Execution class selected for one agent-shell input before runtime mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentShellCommandPlan {
    /// Ordinary slash commands that execute through the synchronous runtime.
    Immediate,
    /// A non-command user prompt submitted through the ordinary prompt path.
    Prompt,
    /// A command that requires one async host effect before or during execution.
    Awaited(AgentShellAwaitedCommand),
}

/// Agent-shell commands whose concrete effect executor may await host work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentShellAwaitedCommand {
    /// Pane model or routing-model selection.
    Model,
    /// Model-backed conversation compaction queueing.
    Compact,
    /// Model-backed durable-memory extraction.
    Remember,
    /// MCP listing after live transport discovery.
    ListMcp,
}

/// Classifies one agent-shell input once before selecting an executor.
fn agent_shell_command_plan(input: &str) -> AgentShellCommandPlan {
    let invocation = parse_slash_command(input).ok().flatten();
    match invocation
        .as_ref()
        .map(|invocation| invocation.name.as_str())
    {
        Some("model") => AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::Model),
        Some("compact") => AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::Compact),
        Some("remember") => AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::Remember),
        Some("list-mcp") => AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::ListMcp),
        Some(_) => AgentShellCommandPlan::Immediate,
        None if input.trim().is_empty() => AgentShellCommandPlan::Immediate,
        None => AgentShellCommandPlan::Prompt,
    }
}

/// Runs the agent shell invalid command response json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_shell_invalid_command_response_json(
    pane_id: &str,
    input: &str,
    error: &MezError,
) -> String {
    let command = input
        .split_whitespace()
        .next()
        .unwrap_or("/")
        .trim_start_matches('/')
        .to_string();
    let outcome = AgentShellCommandOutcome::Display {
        command,
        body: format!(
            "agent command error: {} ({})",
            error.message(),
            runtime_mezzanine_error_code(error.kind())
        ),
    };
    runtime_agent_shell_command_response_json(pane_id, input, Some(&outcome))
}

impl RuntimeSessionService {
    pub(crate) fn toggle_active_agent_shell(
        &mut self,
    ) -> Result<(String, String, AgentShellVisibility)> {
        let pane_id = self.active_pane_id()?;
        let visible = self
            .agent_shell_store()
            .get(&pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
        let (conversation_id, visibility) = if visible {
            let exit = self.request_agent_shell_exit_for_pane(&pane_id)?;
            (exit.conversation_id, exit.visibility)
        } else {
            (
                self.enter_agent_mode_for_pane(&pane_id)?,
                AgentShellVisibility::Visible,
            )
        };
        self.checkpoint_agent_session_metadata()?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","conversation_id":"{}","visible":{}}}"#,
                json_escape(&pane_id),
                json_escape(&conversation_id),
                visibility == AgentShellVisibility::Visible
            ),
        )?;
        Ok((pane_id, conversation_id, visibility))
    }

    /// Requests agent-shell exit while honoring the stop-before-hide contract.
    ///
    /// # Parameters
    /// - `pane_id`: The pane-local agent shell session to hide.
    pub(crate) fn request_agent_shell_exit_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<RuntimeAgentShellExit> {
        let parent_agent_id = format!("agent-{pane_id}");
        self.close_subagent_descendants_for_parent_agent(
            &parent_agent_id,
            "parent agent shell exited",
        )?;
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("agent shell session not found for pane"))?;
        let running_turn_id = self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.clone());
        if running_turn_id.is_some() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: stopping active turn before exiting agent shell; pane input is blocked until stop completes",
            )?;
            self.agent_shell_store_mut()
                .request_hide_pending_task_completion(pane_id)?;
            let stopped = self.stop_agent_turn_for_pane(pane_id)?;
            return Ok(RuntimeAgentShellExit {
                conversation_id,
                visibility: stopped.visibility,
                stopped_turn_id: Some(stopped.turn_id),
            });
        }

        let session = self.agent_shell_store_mut().request_exit(pane_id)?;
        let conversation_id = session.session_id.clone();
        self.advance_pane_shell_prompt_after_agent_exit(pane_id)?;
        self.sync_tracked_pty_sizes()?;
        Ok(RuntimeAgentShellExit {
            conversation_id,
            visibility: AgentShellVisibility::Hidden,
            stopped_turn_id: None,
        })
    }

    /// Shows the pane-local agent prompt and applies live pane side effects.
    ///
    /// The helper is used by both explicit agent-mode entry and runtime-created
    /// agent panes. It keeps the persisted shell-session visibility, prompt
    /// history, scoped child shell, and tracked PTY size in sync before agent
    /// work can run in the pane.
    pub(crate) fn enter_agent_mode_for_pane(&mut self, pane_id: &str) -> Result<String> {
        let conversation_id = self
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)?
            .session_id
            .clone();
        self.reload_agent_prompt_history_for_pane(pane_id)?;
        self.enter_agent_subshell_if_needed(pane_id)?;
        self.clear_agent_shell_terminal_view(pane_id)?;
        self.sync_tracked_pty_sizes()?;
        self.checkpoint_agent_session_metadata()?;
        Ok(conversation_id)
    }

    /// Runs the execute agent shell command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn execute_agent_shell_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
    ) -> Result<String> {
        self.execute_agent_shell_command_with_display(primary_client_id, input, input)
    }

    /// Executes an agent prompt submission while allowing a collapsed display
    /// form for pane transcript rendering.
    pub(crate) fn execute_agent_shell_command_with_display(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
        display_input: &str,
    ) -> Result<String> {
        self.execute_agent_shell_command_with_display_inner(
            primary_client_id,
            input,
            display_input,
            false,
        )
    }

    /// Executes an agent prompt submission while allowing a collapsed display
    /// form for pane transcript rendering.
    fn execute_agent_shell_command_with_display_inner(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
        display_input: &str,
        queue_external_effects_for_adapter: bool,
    ) -> Result<String> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let pane_id = self.active_pane_id()?;
        let visible = self
            .agent_shell_store()
            .get(&pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
        if !visible {
            return Err(MezError::invalid_state(
                "agent shell prompt requires a visible agent shell session",
            ));
        }
        let slash_invocation = parse_slash_command(input).ok().flatten();
        if slash_invocation
            .as_ref()
            .is_some_and(|invocation| invocation.name == "list-mcp")
        {
            self.ensure_runtime_mcp_transports_discovered_blocking()?;
        }
        let is_prompt = !input.trim().is_empty() && !input.trim().starts_with('/');
        self.persist_agent_prompt_history_entry(
            &pane_id,
            input,
            queue_external_effects_for_adapter,
        )?;
        if is_prompt {
            self.append_agent_user_prompt_to_terminal_buffer(&pane_id, display_input)?;
        }
        if let Some(invocation) = parse_macro_prompt_invocation(input) {
            let catalog = self.effective_macro_catalog_for_pane(&pane_id);
            if catalog.get(&invocation.name).is_none() {
                let body = format!(
                    "agent macro error: unknown macro `#{}`. Run `/list-macros` to see available macros.",
                    invocation.name
                );
                let outcome = AgentShellCommandOutcome::Display {
                    command: "macro".to_string(),
                    body,
                };
                return Ok(runtime_agent_shell_command_response_json(
                    &pane_id,
                    input,
                    Some(&outcome),
                ));
            }
        }
        let mcp_summary = self.mcp_registry().agent_shell_summary();
        let permission_summary = self.permission_policy().agent_shell_summary();
        let outcome = match execute_agent_shell_command_with_context(
            self.agent_shell_store_mut(),
            &pane_id,
            input,
            AgentShellRuntimeContext {
                mcp_summary: Some(&mcp_summary),
                permission_summary: Some(&permission_summary),
            },
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Ok(agent_shell_invalid_command_response_json(
                    &pane_id, input, &error,
                ));
            }
        };
        let exit_requires_runtime = outcome.as_ref().is_some_and(|outcome| {
            matches!(
                outcome,
                AgentShellCommandOutcome::RequiresRuntime { command, .. } if command == "exit"
            )
        });
        let response = match (|| -> Result<String> {
            let response =
                if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "exit"
                {
                    let exit = self.request_agent_shell_exit_for_pane(&pane_id)?;
                    let exit_outcome = AgentShellCommandOutcome::Mutated {
                        command: "exit".to_string(),
                        body: format!(
                            "pane={} session={} visibility={} stopped_turn={}",
                            pane_id,
                            exit.conversation_id,
                            agent_shell_visibility_json_name(exit.visibility),
                            exit.stopped_turn_id.as_deref().unwrap_or("none")
                        ),
                        visibility: exit.visibility,
                    };
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&exit_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "stop"
                {
                    let stopped = self.stop_agent_turn_for_pane(&pane_id)?;
                    runtime_agent_shell_stop_response_json(&pane_id, input, &stopped)
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "model"
                {
                    let model_outcome = self.execute_agent_shell_model_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&model_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "latency"
                {
                    let latency_outcome =
                        self.execute_agent_shell_latency_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&latency_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "thinking"
                {
                    let thinking_outcome =
                        self.execute_agent_shell_thinking_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&thinking_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "compact"
                {
                    let compact_outcome =
                        self.execute_agent_shell_compact_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&compact_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "routing"
                {
                    let routing_outcome =
                        self.execute_agent_shell_routing_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&routing_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "personality"
                {
                    let personality_outcome =
                        self.execute_agent_shell_personality_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&personality_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "resume"
                {
                    let resume_outcome =
                        self.execute_agent_shell_resume_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&resume_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "fork"
                {
                    let fork_outcome =
                        self.execute_agent_shell_fork_command(primary_client_id, &pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&fork_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "list-sessions"
                {
                    let sessions_outcome =
                        self.execute_agent_shell_list_sessions_command(&pane_id)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&sessions_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "list-macros"
                {
                    let macros_outcome = self.execute_agent_shell_list_macros_command(&pane_id)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&macros_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "list-skills"
                {
                    let skills_outcome = self.execute_agent_shell_list_skills_command(&pane_id)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&skills_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "sync-builtin-skills"
                {
                    let skills_outcome = self.execute_agent_shell_sync_builtin_skills_command()?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&skills_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "list-modified-files"
                {
                    let modified_outcome =
                        self.execute_agent_shell_list_modified_files_command(&pane_id);
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&modified_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "copy-context"
                {
                    let context_outcome =
                        self.execute_agent_shell_copy_context_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&context_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "copy-trace-log"
                {
                    let trace_outcome =
                        self.execute_agent_shell_copy_trace_log_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&trace_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "copy-patches"
                {
                    let patches_outcome =
                        self.execute_agent_shell_copy_patches_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&patches_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "debug-config"
                {
                    let debug_outcome = self.execute_agent_shell_debug_config_command(input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&debug_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "diff"
                {
                    let diff_outcome = self.execute_agent_shell_diff_command(&pane_id)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&diff_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "init"
                {
                    let init_outcome = self.execute_agent_shell_init_command(
                        &pane_id,
                        input,
                        queue_external_effects_for_adapter,
                    )?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&init_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "copy"
                {
                    let copy_outcome = self.execute_agent_shell_copy_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&copy_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "logout"
                {
                    let logout_outcome = self.execute_agent_shell_logout_command(&pane_id)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&logout_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "permissions"
                {
                    let permissions_outcome =
                        self.execute_agent_shell_permissions_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&permissions_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "approval"
                {
                    let approval_outcome =
                        self.execute_agent_shell_approval_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&approval_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "approve"
                {
                    let approve_outcome = self.execute_agent_shell_approve_command(
                        primary_client_id,
                        &pane_id,
                        input,
                    )?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&approve_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "trust"
                {
                    let trust_outcome =
                        self.execute_agent_shell_trust_command(primary_client_id, &pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&trust_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "memory"
                {
                    let memory_outcome =
                        self.execute_agent_shell_memory_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&memory_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "issue"
                {
                    let issue_outcome =
                        issues::execute_agent_shell_issue_command(self, &pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&issue_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "show-issues"
                {
                    let show_outcome =
                        self.execute_agent_shell_show_issues_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&show_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "show-memories"
                {
                    let show_outcome =
                        self.execute_agent_shell_show_memories_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&show_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "remember"
                {
                    let remember_outcome =
                        self.execute_agent_shell_remember_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&remember_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "title"
                {
                    let title_outcome =
                        self.execute_agent_shell_title_command(primary_client_id, &pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&title_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "loop"
                {
                    let loop_outcome = self.execute_agent_shell_loop_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&loop_outcome))
                } else if let Some(AgentShellCommandOutcome::Display { command, .. }) =
                    outcome.as_ref()
                    && command == "status"
                {
                    let status_outcome = self.execute_agent_shell_status_command(&pane_id)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&status_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::Mutated { command, .. }) =
                    outcome.as_ref()
                    && matches!(command.as_str(), "clear" | "new")
                {
                    let cleared = self.clear_agent_shell_terminal_view(&pane_id)?;
                    let mut clear_outcome = outcome.as_ref().cloned().ok_or_else(|| {
                        MezError::invalid_state("clear/new command outcome was missing")
                    })?;
                    if let AgentShellCommandOutcome::Mutated { body, .. } = &mut clear_outcome {
                        body.push_str(&format!(" terminal_view_cleared={cleared}"));
                    }
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&clear_outcome))
                } else if let Some(outcome) = outcome.as_ref() {
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(outcome))
                } else {
                    if let Some(turn_id) =
                        self.inject_agent_steering_for_running_turn(&pane_id, input)?
                    {
                        let visibility = self.agent_shell_visibility_for_pane(&pane_id)?;
                        let steer_outcome = AgentShellCommandOutcome::Mutated {
                            command: "prompt".to_string(),
                            body: format!(
                                "pane={} agent_prompt_turn={} injected_user_input=true",
                                pane_id, turn_id
                            ),
                            visibility,
                        };
                        runtime_agent_shell_command_response_json(
                            &pane_id,
                            input,
                            Some(&steer_outcome),
                        )
                    } else if parse_macro_prompt_invocation(input).is_some() {
                        let started = self.start_agent_macro_prompt_turn(&pane_id, input)?;
                        runtime_agent_shell_prompt_turn_response_json(&pane_id, input, &started)
                    } else {
                        let started = self.start_agent_prompt_turn(&pane_id, input)?;
                        runtime_agent_shell_prompt_turn_response_json(&pane_id, input, &started)
                    }
                };
            Ok(response)
        })() {
            Ok(response) => response,
            Err(error) => agent_shell_invalid_command_response_json(&pane_id, input, &error),
        };
        if let Some(AgentShellCommandOutcome::Mutated { command, .. }) = outcome.as_ref()
            && matches!(command.as_str(), "new" | "clear")
        {
            self.clear_agent_modified_files(&pane_id);
            self.reload_agent_prompt_history_for_pane(&pane_id)?;
        }
        if exit_requires_runtime
            && self
                .agent_shell_store()
                .get(&pane_id)
                .is_some_and(|session| session.visibility == AgentShellVisibility::Hidden)
        {
            self.advance_pane_shell_prompt_after_agent_exit(&pane_id)?;
        }
        if outcome.is_some() {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_shell_command":"{}"}}"#,
                    json_escape(&pane_id),
                    json_escape(input)
                ),
            )?;
        }
        self.checkpoint_agent_session_metadata()?;
        Ok(response)
    }

    /// Starts any configured MCP servers before a synchronous `/list-mcp`.
    ///
    /// The normal async runtime path performs this work directly. The blocking
    /// path exists for foreground/control helpers that still execute
    /// agent-shell commands through the synchronous service API.
    fn ensure_runtime_mcp_transports_discovered_blocking(&mut self) -> Result<()> {
        let needs_discovery = self
            .mcp_registry()
            .list_servers()
            .into_iter()
            .any(|server| {
                server.configured.enabled
                    && server.status == mez_agent::mcp::McpServerStatus::Configured
            });
        if !needs_discovery {
            return Ok(());
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            return Ok(());
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                MezError::invalid_state(format!("failed to create MCP discovery runtime: {error}"))
            })?;
        runtime
            .block_on(self.ensure_runtime_mcp_transports_discovered_async())
            .map(|_| ())
    }

    /// Executes `/remember` from synchronous UI paths by queuing model work.
    fn execute_agent_shell_remember_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        self.queue_agent_shell_remember_command_with_model(pane_id, input)
    }

    /// Executes one agent-shell input through a typed sync/awaited plan.
    pub async fn execute_agent_shell_command_async(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
    ) -> Result<String> {
        let plan = agent_shell_command_plan(input);
        let AgentShellCommandPlan::Awaited(awaited_command) = plan else {
            return self.execute_agent_shell_command_with_display_inner(
                primary_client_id,
                input,
                input,
                true,
            );
        };

        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let pane_id = self.active_pane_id()?;
        let visible = self
            .agent_shell_store()
            .get(&pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
        if !visible {
            return Err(MezError::invalid_state(
                "agent shell prompt requires a visible agent shell session",
            ));
        }
        if awaited_command == AgentShellAwaitedCommand::ListMcp {
            self.ensure_runtime_mcp_transports_discovered_async()
                .await?;
        }

        self.persist_agent_prompt_history_entry(&pane_id, input, true)?;
        let mcp_summary = self.mcp_registry().agent_shell_summary();
        let permission_summary = self.permission_policy().agent_shell_summary();
        let outcome = match execute_agent_shell_command_with_context(
            self.agent_shell_store_mut(),
            &pane_id,
            input,
            AgentShellRuntimeContext {
                mcp_summary: Some(&mcp_summary),
                permission_summary: Some(&permission_summary),
            },
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                return Ok(agent_shell_invalid_command_response_json(
                    &pane_id, input, &error,
                ));
            }
        };

        let response = match async {
            let runtime_outcome = match awaited_command {
                AgentShellAwaitedCommand::Model => {
                    self.execute_agent_shell_model_command(&pane_id, input)?
                }
                AgentShellAwaitedCommand::Compact => {
                    self.execute_agent_shell_compact_command(&pane_id, input)?
                }
                AgentShellAwaitedCommand::Remember => {
                    self.execute_agent_shell_remember_command_async(&pane_id, input)
                        .await?
                }
                AgentShellAwaitedCommand::ListMcp => {
                    return Ok(runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        outcome.as_ref(),
                    ));
                }
            };
            Ok(runtime_agent_shell_command_response_json(
                &pane_id,
                input,
                Some(&runtime_outcome),
            ))
        }
        .await
        {
            Ok(response) => response,
            Err(error) => agent_shell_invalid_command_response_json(&pane_id, input, &error),
        };
        if outcome.is_some() {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_shell_command":"{}"}}"#,
                    json_escape(&pane_id),
                    json_escape(input)
                ),
            )?;
        }
        self.checkpoint_agent_session_metadata()?;
        Ok(response)
    }

    /// Starts the configured shell as a child shell for an agent-mode pane.
    ///
    /// The child shell inherits the pane's current directory. Shell commands
    /// issued by the agent can mutate that child, but leaving agent mode returns
    /// to the original interactive shell without inheriting prompt, option, or
    /// environment changes made inside the agent context.
    pub(crate) fn enter_agent_subshell_if_needed(&mut self, pane_id: &str) -> Result<bool> {
        if self.agent_subshell_is_active(pane_id)
            || self.primary_pid_for_live_pane_process(pane_id).is_none()
        {
            return Ok(false);
        }
        let shell_command = agent_subshell_enter_command(
            self.session.shell.path(),
            self.shell_classification_for_pane(pane_id),
        )?;
        match self.write_runtime_pane_input(pane_id, shell_command.as_bytes()) {
            Ok(()) => {
                self.enter_agent_subshell(pane_id);
                self.take_agent_subshell_command_exit(pane_id);
                self.remember_hidden_shell_render_suppression(pane_id);
                Ok(true)
            }
            Err(error)
                if error.kind() == MezErrorKind::NotFound
                    || matches!(
                        error.io_kind(),
                        Some(std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected)
                    ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Leaves the child shell created for agent mode when it is safe to do so.
    ///
    /// If a turn or shell transaction is still active, the subshell remains in
    /// place until the turn finishes so follow-up model actions cannot leak into
    /// the user's parent shell.
    pub(crate) fn exit_agent_subshell_if_active(&mut self, pane_id: &str) -> Result<bool> {
        if !self.agent_subshell_is_active(pane_id) {
            return Ok(false);
        }
        if self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
            || self.pane_has_running_shell_transaction(pane_id)
        {
            return Ok(false);
        }
        if self.primary_pid_for_live_pane_process(pane_id).is_none() {
            self.clear_agent_subshell_state(pane_id);
            self.clear_shell_output_filters_for_foreground_input(pane_id);
            return Ok(false);
        }
        self.clear_shell_output_filters_for_foreground_input(pane_id);
        let input = if self.take_agent_subshell_command_exit(pane_id) {
            b"exit\n".as_slice()
        } else {
            b"\x04".as_slice()
        };
        match self.write_runtime_pane_input(pane_id, input) {
            Ok(()) => {
                self.leave_agent_subshell(pane_id);
                Ok(true)
            }
            Err(error)
                if error.kind() == MezErrorKind::NotFound
                    || matches!(
                        error.io_kind(),
                        Some(std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected)
                    ) =>
            {
                self.clear_agent_subshell_state(pane_id);
                self.clear_shell_output_filters_for_foreground_input(pane_id);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Clears the live viewport and advances the pane shell prompt after agent exit.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn advance_pane_shell_prompt_after_agent_exit(
        &mut self,
        pane_id: &str,
    ) -> Result<bool> {
        let cleared = self.clear_agent_shell_terminal_view(pane_id)?;
        let advanced = self.exit_agent_subshell_if_active(pane_id)?;
        Ok(cleared || advanced)
    }

    /// Runs the persist agent prompt history entry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn persist_agent_prompt_history_entry(
        &mut self,
        pane_id: &str,
        input: &str,
        queue_for_adapter: bool,
    ) -> Result<()> {
        if input.trim().is_empty() {
            return Ok(());
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(());
        };
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(());
        };
        if queue_for_adapter {
            self.persistence
                .queue_transcript(RuntimeSideEffect::PersistPromptHistory {
                    path: store.prompt_history_file(),
                    store,
                    conversation_id: session.session_id.clone(),
                    prompt: input.to_string(),
                });
            return Ok(());
        }
        let _ = store.append_prompt_history(&session.session_id, input)?;
        Ok(())
    }
}

#[cfg(test)]
mod plan_tests {
    use super::{AgentShellAwaitedCommand, AgentShellCommandPlan, agent_shell_command_plan};

    /// Verifies model, compaction, memory extraction, and MCP discovery are
    /// classified as the only agent-shell inputs requiring async host work.
    #[test]
    fn agent_shell_plan_identifies_awaited_commands() {
        assert_eq!(
            agent_shell_command_plan("/model --routing show"),
            AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::Model)
        );
        assert_eq!(
            agent_shell_command_plan("/compact"),
            AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::Compact)
        );
        assert_eq!(
            agent_shell_command_plan("/remember"),
            AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::Remember)
        );
        assert_eq!(
            agent_shell_command_plan("/list-mcp"),
            AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::ListMcp)
        );
    }

    /// Verifies user prompts and ordinary slash commands remain distinct typed
    /// plans while sharing the serialized immediate runtime executor.
    #[test]
    fn agent_shell_plan_separates_prompts_from_immediate_commands() {
        assert_eq!(
            agent_shell_command_plan("continue the implementation"),
            AgentShellCommandPlan::Prompt
        );
        assert_eq!(
            agent_shell_command_plan("/status"),
            AgentShellCommandPlan::Immediate
        );
    }
}
