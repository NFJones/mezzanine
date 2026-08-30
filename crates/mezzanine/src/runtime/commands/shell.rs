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
    execute_agent_shell_command_with_context, json_escape, parse_slash_command,
    runtime_agent_shell_command_response_json, runtime_agent_shell_prompt_turn_response_json,
    runtime_agent_shell_stop_response_json, runtime_mezzanine_error_code,
};
use crate::integrations::agent::slash::AgentShellPresentation;
use crate::runtime::{PaneReadinessState, runtime_random_marker_token};
use crate::{error::MezErrorKind, runtime::commands::issues};
use mez_agent::{
    ShellClassification, agent_subshell_enter_command_with_shell_compatibility_and_exit_marker,
    agent_subshell_exit_marker_bytes, bash_private_handoff_source_input,
    parse_macro_prompt_invocation,
};
use mez_mux::readline::ReadlineHistoryEntry;

/// Authenticated provenance carried with one live agent-shell command.
///
/// This value is assigned by runtime ingress rather than parsed from command
/// text. Security-sensitive commands must explicitly accept the origins that
/// are allowed to mutate host-backed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentShellCommandOrigin {
    /// Input submitted through the attached primary terminal or its UI.
    AuthenticatedPrimaryInput,
    /// Input submitted through the authenticated control protocol.
    AuthenticatedControlRequest,
}

impl AgentShellCommandOrigin {
    /// Returns whether this origin proves a direct primary-client UI event.
    pub(crate) const fn is_authenticated_primary_input(self) -> bool {
        matches!(self, Self::AuthenticatedPrimaryInput)
    }
}

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
    /// Provider catalog refresh through the async runtime.
    RefreshProviderInfo,
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
        Some("refresh-provider-info") => {
            AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::RefreshProviderInfo)
        }
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
        self.clear_deferred_agent_subshell_entry(pane_id);
        let running_turn_id = self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.clone());
        if running_turn_id.is_some() {
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
        self.enter_agent_mode_for_pane_with_origin(pane_id, false)
    }

    /// Shows agent mode for a pane whose execution backend was selected before launch.
    pub(crate) fn enter_runtime_owned_agent_mode_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<String> {
        self.enter_agent_mode_for_pane_with_origin(pane_id, true)
    }

    /// Applies common agent presentation while keeping foreign-shell entry
    /// exclusive to existing user-owned panes.
    fn enter_agent_mode_for_pane_with_origin(
        &mut self,
        pane_id: &str,
        runtime_owned: bool,
    ) -> Result<String> {
        let conversation_id = self
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)?
            .session_id
            .clone();
        self.reload_agent_prompt_history_for_pane(pane_id)?;
        if runtime_owned {
            if self.runtime_agent_surface_startup(pane_id).is_none() {
                return Err(MezError::invalid_state(
                    "runtime-owned agent pane is missing its startup owner",
                ));
            }
        } else {
            self.enter_agent_subshell_if_needed(pane_id)?;
        }
        self.sync_tracked_pty_sizes()?;
        let size = self
            .tracked_pane_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.pane_id.as_str() == pane_id)
            .map(|descriptor| descriptor.size)
            .or_else(|| {
                self.process_pane_screen(pane_id)
                    .map(|screen| screen.size())
            })
            .or_else(|| self.find_pane_descriptor(pane_id).map(|pane| pane.size))
            .ok_or_else(|| MezError::invalid_state("agent pane screen size is unavailable"))?;
        self.ensure_agent_pane_screen(pane_id, &conversation_id, size)?;
        if runtime_owned
            && self.effective_agent_shell_mode_for_pane(pane_id)
                == crate::runtime::config::ShellMode::Native
        {
            self.native_shell_context_for_pane(pane_id)?;
            if !self.complete_native_agent_surface_startup(pane_id) {
                return Err(MezError::invalid_state(
                    "native runtime-owned agent startup is not awaiting validation",
                ));
            }
        }
        self.checkpoint_agent_session_metadata()?;
        self.request_agent_prompt_selector_extra_candidates_refresh(pane_id);
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
        self.execute_agent_shell_command_with_origin(
            primary_client_id,
            input,
            input,
            AgentShellCommandOrigin::AuthenticatedPrimaryInput,
            false,
            ReadlineHistoryEntry::literal(input),
        )
    }

    /// Captures an interactive provider refresh for actor-owned asynchronous execution.
    pub(crate) fn prepare_agent_prompt_provider_info_refresh(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<Option<crate::runtime::RuntimeAgentPromptProviderInfoRefresh>> {
        let Some(work) =
            self.prepare_agent_shell_provider_info_refresh(primary_client_id, input)?
        else {
            return Ok(None);
        };
        Ok(Some(
            crate::runtime::RuntimeAgentPromptProviderInfoRefresh {
                primary_client_id: primary_client_id.clone(),
                pane_id: pane_id.to_string(),
                input: input.to_string(),
                work: Some(work),
            },
        ))
    }

    /// Executes an agent-shell command submitted through the control protocol.
    pub(crate) fn execute_agent_shell_control_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
    ) -> Result<String> {
        self.execute_agent_shell_command_with_origin(
            primary_client_id,
            input,
            input,
            AgentShellCommandOrigin::AuthenticatedControlRequest,
            false,
            ReadlineHistoryEntry::literal(input),
        )
    }

    /// Executes an agent prompt submission while allowing a collapsed display
    /// form for pane transcript rendering.
    pub(crate) fn execute_agent_shell_command_with_display(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
        display_input: &str,
        collapsed_paste_ranges: &[mez_mux::readline::ReadlinePasteRange],
    ) -> Result<String> {
        self.execute_agent_shell_command_with_origin(
            primary_client_id,
            input,
            display_input,
            AgentShellCommandOrigin::AuthenticatedPrimaryInput,
            false,
            ReadlineHistoryEntry {
                text: input.to_string(),
                collapsed_paste_ranges: collapsed_paste_ranges.to_vec(),
            },
        )
    }

    /// Executes an agent prompt submission while allowing a collapsed display
    /// form for pane transcript rendering.
    fn execute_agent_shell_command_with_origin(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
        display_input: &str,
        origin: AgentShellCommandOrigin,
        queue_external_effects_for_adapter: bool,
        history_entry: ReadlineHistoryEntry,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
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
            &history_entry,
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
        let permission_summary = self
            .permission_policy_for_pane(&pane_id)
            .agent_shell_summary();
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
                    && command == "auth-status"
                {
                    let auth_outcome = self.execute_agent_shell_auth_status_command(input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&auth_outcome))
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
                    && command == "plan"
                {
                    let plan_outcome = self.execute_agent_shell_plan_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&plan_outcome))
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
                    && command == "list-personalities"
                {
                    let personalities_outcome =
                        self.execute_agent_shell_list_personalities_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&personalities_outcome),
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
                    && command == "name-session"
                {
                    let name_outcome =
                        self.execute_agent_shell_name_session_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&name_outcome))
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
                    && command == "sandbox"
                {
                    let sandbox_outcome = self
                        .execute_agent_shell_sandbox_command(
                            primary_client_id,
                            &pane_id,
                            input,
                            origin,
                        )
                        .unwrap_or_else(|error| AgentShellCommandOutcome::Presented {
                            command: "sandbox".to_string(),
                            body: format!("Sandbox error: {}", error.message()),
                            presentation: AgentShellPresentation::ErrorNotice,
                        });
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&sandbox_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "shell-mode"
                {
                    let shell_mode_outcome = self
                        .execute_agent_shell_shell_mode_command(
                            primary_client_id,
                            &pane_id,
                            input,
                            origin,
                        )
                        .unwrap_or_else(|error| AgentShellCommandOutcome::Presented {
                            command: "shell-mode".to_string(),
                            body: format!("Shell mode error: {}", error.message()),
                            presentation: AgentShellPresentation::ErrorNotice,
                        });
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&shell_mode_outcome),
                    )
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
                    && command == "editor-recovery"
                {
                    let recovery_outcome = self.execute_agent_shell_editor_recovery_command(
                        primary_client_id,
                        &pane_id,
                        input,
                    )?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&recovery_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "show-approvals"
                {
                    let show_outcome =
                        self.execute_agent_shell_show_approvals_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&show_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "show-context"
                {
                    let show_outcome =
                        self.execute_agent_shell_show_context_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&show_outcome))
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
                    && command == "loop"
                {
                    let loop_outcome = self.execute_agent_shell_loop_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&loop_outcome))
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "reset-status"
                {
                    let reset_outcome = self.execute_agent_shell_reset_status_command(&pane_id)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&reset_outcome))
                } else if let Some(AgentShellCommandOutcome::Display { command, .. }) =
                    outcome.as_ref()
                    && command == "status"
                {
                    let status_outcome =
                        self.execute_agent_shell_status_command(&pane_id, input)?;
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
        if outcome.is_some() && !exit_requires_runtime {
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

    /// Executes explicit external-editor recovery operations for the active pane.
    fn execute_agent_shell_editor_recovery_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("editor-recovery command must be a slash command")
        })?;
        let arguments = invocation.args.split_whitespace().collect::<Vec<_>>();
        match arguments.as_slice() {
            [] | ["list"] => Ok(AgentShellCommandOutcome::Display {
                command: "editor-recovery".to_string(),
                body: self.list_external_editor_recoveries(primary_client_id)?,
            }),
            ["apply", session_id] => {
                self.apply_external_editor_recovery(primary_client_id, pane_id, session_id)?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "editor-recovery".to_string(),
                    body: format!("recovery={} action=apply changed=true", session_id),
                    visibility: self.agent_shell_visibility_for_pane(pane_id)?,
                })
            }
            ["reopen", session_id] => {
                self.reopen_external_editor_recovery(primary_client_id, pane_id, session_id)?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "editor-recovery".to_string(),
                    body: format!("recovery={} action=reopen changed=true", session_id),
                    visibility: self.agent_shell_visibility_for_pane(pane_id)?,
                })
            }
            ["discard", session_id] => {
                let changed =
                    self.discard_external_editor_recovery(primary_client_id, pane_id, session_id)?;
                Ok(AgentShellCommandOutcome::Mutated {
                    command: "editor-recovery".to_string(),
                    body: format!("recovery={} action=discard changed={changed}", session_id),
                    visibility: self.agent_shell_visibility_for_pane(pane_id)?,
                })
            }
            _ => Err(MezError::invalid_args(
                "editor-recovery expects list, apply <id>, reopen <id>, or discard <id>",
            )),
        }
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
            return self.execute_agent_shell_command_with_origin(
                primary_client_id,
                input,
                input,
                AgentShellCommandOrigin::AuthenticatedPrimaryInput,
                true,
                ReadlineHistoryEntry::literal(input),
            );
        };

        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
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

        self.persist_agent_prompt_history_entry(
            &pane_id,
            &ReadlineHistoryEntry::literal(input),
            true,
        )?;
        let mcp_summary = self.mcp_registry().agent_shell_summary();
        let permission_summary = self
            .permission_policy_for_pane(&pane_id)
            .agent_shell_summary();
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
                AgentShellAwaitedCommand::RefreshProviderInfo => {
                    self.execute_agent_shell_refresh_provider_info_command(input)
                        .await?
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

    /// Prepares lazy MCP discovery before an actor-owned `/list-mcp` command.
    ///
    /// Authorization and visible-shell checks remain serialized. Only the
    /// transport startup work crosses to the worker, and other commands return
    /// `None` so they continue through the ordinary command path.
    pub(crate) fn prepare_agent_shell_mcp_discovery(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
    ) -> Result<Option<crate::runtime::RuntimeAgentProviderPreparationWork>> {
        if agent_shell_command_plan(input)
            != AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::ListMcp)
        {
            return Ok(None);
        }
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
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
        self.prepare_runtime_mcp_discovery_work().map(Some)
    }

    /// Prepares provider catalog refresh before an actor-owned shell command.
    pub(crate) fn prepare_agent_shell_provider_info_refresh(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
    ) -> Result<Option<crate::runtime::RuntimeProviderInfoRefreshWork>> {
        if agent_shell_command_plan(input)
            != AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::RefreshProviderInfo)
        {
            return Ok(None);
        }
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        let pane_id = self.active_pane_id()?;
        if self
            .agent_shell_store()
            .get(&pane_id)
            .is_none_or(|session| session.visibility != AgentShellVisibility::Visible)
        {
            return Err(MezError::invalid_state(
                "agent shell prompt requires a visible agent shell session",
            ));
        }
        self.validate_agent_shell_refresh_provider_info_command(input)?;
        self.prepare_provider_info_refresh().map(Some)
    }

    /// Completes an actor-owned provider refresh shell command.
    pub(crate) fn complete_agent_shell_provider_info_refresh(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        input: &str,
        outcome: crate::runtime::RuntimeProviderInfoRefreshOutcome,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        self.session.activate_client_navigation(primary_client_id)?;
        let pane_id = self.active_pane_id()?;
        if self
            .agent_shell_store()
            .get(&pane_id)
            .is_none_or(|session| session.visibility != AgentShellVisibility::Visible)
        {
            return Err(MezError::invalid_state(
                "agent shell prompt requires a visible agent shell session",
            ));
        }
        self.persist_agent_prompt_history_entry(
            &pane_id,
            &ReadlineHistoryEntry::literal(input),
            true,
        )?;
        let body = self.apply_provider_info_refresh(outcome)?;
        let command_outcome = AgentShellCommandOutcome::Display {
            command: "refresh-provider-info".to_string(),
            body,
        };
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_shell_command":"{}"}}"#,
                json_escape(&pane_id),
                json_escape(input)
            ),
        )?;
        self.checkpoint_agent_session_metadata()?;
        Ok(runtime_agent_shell_command_response_json(
            &pane_id,
            input,
            Some(&command_outcome),
        ))
    }

    /// Starts the configured shell as a child shell for an agent-mode pane.
    ///
    /// The child shell inherits the pane's current directory. Shell commands
    /// issued by the agent can mutate that child, but leaving agent mode returns
    /// to the original interactive shell without inheriting prompt, option, or
    /// environment changes made inside the agent context.
    ///
    /// Shell discovery and bootstrap begin only after this explicit entry
    /// point. An unmanaged parent is generation-fenced and handed to the
    /// dependency-free loader, which installs compatibility only in the
    /// agent-owned child before releasing the bootstrap payload.
    pub(crate) fn enter_agent_subshell_if_needed(&mut self, pane_id: &str) -> Result<bool> {
        if self.effective_agent_shell_mode_for_pane(pane_id)
            == crate::runtime::config::ShellMode::Native
        {
            self.clear_deferred_agent_subshell_entry(pane_id);
            self.clear_pane_bootstrap_pending(pane_id);
            self.native_shell_context_for_pane(pane_id)?;
            return Ok(false);
        }
        if self.agent_subshell_is_active(pane_id)
            || self.primary_pid_for_live_pane_process(pane_id).is_none()
        {
            return Ok(false);
        }
        if self.managed_shell_handoff_is_pending(pane_id) {
            self.defer_agent_subshell_entry(pane_id);
            return Ok(false);
        }
        let legacy_managed_startup = self.legacy_managed_startup_is_enabled();
        let legacy_managed_parent = legacy_managed_startup
            && matches!(
                self.shell_classification_for_pane(pane_id),
                ShellClassification::Bash | ShellClassification::Fish | ShellClassification::Zsh
            );
        if !legacy_managed_parent
            && self.pane_has_unsubmitted_process_input(pane_id)
            && !self.agent_subshell_input_clear_is_pending(pane_id)
        {
            self.defer_agent_subshell_entry(pane_id);
            self.begin_agent_subshell_input_clear(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::InteractiveBlocked);
            self.remember_hidden_shell_render_suppression(pane_id);
            match self.write_runtime_pane_input(pane_id, b"\x03") {
                Ok(()) => return Ok(true),
                Err(error) if error.kind() == MezErrorKind::NotFound => {
                    self.clear_agent_subshell_state(pane_id);
                    return Ok(false);
                }
                Err(error) => {
                    self.clear_agent_subshell_state(pane_id);
                    return Err(error);
                }
            }
        }
        if !legacy_managed_parent && self.agent_subshell_input_clear_is_pending(pane_id) {
            self.defer_agent_subshell_entry(pane_id);
            if !matches!(
                self.pane_readiness_state(pane_id),
                PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
            ) {
                return Ok(false);
            }
            self.finish_agent_subshell_input_clear(pane_id);
        }
        if !legacy_managed_startup
            && !self.pane_has_uncertified_foreign_shell_boundary(pane_id)
            && !self.dependency_free_foreign_loader_owns_parent_restoration(pane_id)
            && !self.pane_has_running_shell_transaction(pane_id)
        {
            let foreground_is_observed = self
                .pane_foreground_process_group_observation(pane_id)
                .0
                .is_some();
            let prompt_is_ready = matches!(
                self.pane_readiness_state(pane_id),
                PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
            );
            if self.begin_agent_entry_shell_boundary_for_current_foreground(pane_id) {
                if foreground_is_observed || prompt_is_ready {
                    self.begin_dependency_free_foreign_shell_bootstrap(pane_id)?;
                }
            } else {
                self.defer_agent_subshell_entry(pane_id);
                return Ok(false);
            }
            self.defer_agent_subshell_entry(pane_id);
            return Ok(false);
        }
        if self.pane_has_uncertified_foreign_shell_boundary(pane_id) {
            self.defer_agent_subshell_entry(pane_id);
            return Ok(false);
        }
        if self.pane_bootstrap_is_pending(pane_id)
            && self.pane_has_running_shell_transaction(pane_id)
        {
            self.defer_agent_subshell_entry(pane_id);
            return Ok(false);
        }
        let _ = self.schedule_parent_shell_discovery_for_agent_entry(pane_id);
        if self.pane_bootstrap_awaits_shell_identity(pane_id) {
            self.defer_agent_subshell_entry(pane_id);
            if matches!(
                self.pane_readiness_state(pane_id),
                PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
            ) {
                let _ = self.maybe_bootstrap_ready_panes()?;
            }
            return Ok(false);
        }
        let shell_identity = self.shell_execution_identity_for_pane(pane_id)?;
        let classification = shell_identity.classification();
        if classification == ShellClassification::Bash {
            match self.managed_bash_admission_for_pane(pane_id) {
                Some(crate::runtime::processes::RuntimeManagedBashAdmission::Ready {
                    version,
                    ..
                }) if *version == mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION => {}
                Some(crate::runtime::processes::RuntimeManagedBashAdmission::Pending {
                    ..
                }) => {
                    self.arm_managed_bash_admission_deadline(pane_id);
                    self.defer_agent_subshell_entry(pane_id);
                    return Ok(false);
                }
                Some(crate::runtime::processes::RuntimeManagedBashAdmission::Unavailable {
                    reason,
                }) => {
                    let reason = reason.clone();
                    self.clear_deferred_agent_subshell_entry(pane_id);
                    self.append_agent_status_text_to_terminal_buffer(
                        pane_id,
                        &format!("agent: managed Bash integration unavailable ({reason})"),
                    )?;
                    return Ok(false);
                }
                _ => {
                    self.append_agent_status_text_to_terminal_buffer(
                        pane_id,
                        "agent: managed Bash integration is unavailable",
                    )?;
                    return Ok(false);
                }
            }
        }
        if classification == ShellClassification::Zsh {
            match self.managed_zsh_admission_for_pane(pane_id) {
                Some(crate::runtime::processes::RuntimeManagedZshAdmission::Ready { .. }) => {}
                Some(crate::runtime::processes::RuntimeManagedZshAdmission::Pending { .. }) => {
                    self.arm_managed_zsh_admission_deadline(pane_id);
                    self.defer_agent_subshell_entry(pane_id);
                    return Ok(false);
                }
                Some(crate::runtime::processes::RuntimeManagedZshAdmission::Unavailable {
                    reason,
                }) => {
                    let reason = reason.clone();
                    self.clear_deferred_agent_subshell_entry(pane_id);
                    self.append_agent_status_text_to_terminal_buffer(
                        pane_id,
                        &format!("agent: managed zsh integration unavailable ({reason})"),
                    )?;
                    return Ok(false);
                }
                None => {
                    self.append_agent_status_text_to_terminal_buffer(
                        pane_id,
                        "agent: managed zsh integration is unavailable",
                    )?;
                    return Ok(false);
                }
            }
        }
        let zsh_history_token = self.zsh_history_token_for_pane(pane_id).cloned();
        let managed_zsh = self.managed_zsh_shell_for_pane(pane_id)?;
        let bash_receiver_rcfile = self
            .bash_receiver_rcfile_for_pane(pane_id)
            .map(std::path::Path::to_path_buf);
        let fish_receiver_token = self.fish_receiver_token_for_pane(pane_id).cloned();
        self.begin_agent_subshell_shell_handoff(pane_id)?;
        let prepared_bootstrap = match self.prepare_bootstrap_to_pane(pane_id) {
            Ok(prepared_bootstrap) => prepared_bootstrap,
            Err(error) => {
                self.clear_agent_subshell_shell_identity(pane_id);
                return Err(error);
            }
        };
        let bash_receiver_install_marker = prepared_bootstrap
            .as_ref()
            .map(|(marker, _)| marker.as_str());
        let exit_marker = runtime_random_marker_token(&format!("agent-subshell-exit\0{pane_id}"))?;
        let shell_command = agent_subshell_enter_command_with_shell_compatibility_and_exit_marker(
            shell_identity.shell_path(),
            classification,
            zsh_history_token.as_ref(),
            managed_zsh.as_ref(),
            bash_receiver_rcfile.as_deref(),
            bash_receiver_install_marker,
            fish_receiver_token.as_ref().zip(
                prepared_bootstrap
                    .as_ref()
                    .map(|(marker, _)| marker.as_str()),
            ),
            (classification == ShellClassification::Zsh)
                .then_some(bash_receiver_install_marker)
                .flatten(),
            Some(&exit_marker),
        )?;
        if let Some((marker, wrapper)) = prepared_bootstrap.as_ref() {
            self.bind_agent_subshell_bootstrap_marker(pane_id, marker);
            self.defer_agent_subshell_bootstrap_wrapper(pane_id, marker, wrapper.clone());
        }
        let shell_input = if classification == ShellClassification::Bash {
            let (marker, _) = prepared_bootstrap.as_ref().ok_or_else(|| {
                MezError::invalid_state(
                    "managed Bash subshell handoff requires a registered bootstrap owner",
                )
            })?;
            let token = self
                .bash_receiver_token_for_pane(pane_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "managed Bash receiver is unavailable for agent subshell handoff",
                    )
                })?;
            let parent_proof =
                runtime_random_marker_token(&format!("bash-parent-ready\0{pane_id}\0{marker}"))?;
            let private_input =
                bash_private_handoff_source_input(&shell_command, &token, marker, &parent_proof);
            self.prepend_bash_shell_handoff_payload(
                marker,
                mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    private_input.receiver_payload.into_bytes(),
                    marker.clone(),
                    true,
                ),
                &parent_proof,
            );
            private_input.wrapper
        } else if classification == ShellClassification::Fish {
            let (marker, _) = prepared_bootstrap.as_ref().ok_or_else(|| {
                MezError::invalid_state(
                    "managed Fish subshell handoff requires a registered bootstrap owner",
                )
            })?;
            let token = fish_receiver_token.ok_or_else(|| {
                MezError::invalid_state(
                    "managed Fish receiver is unavailable for agent subshell handoff",
                )
            })?;
            let private_input =
                mez_agent::fish_private_source_input(&shell_command, &token, marker);
            self.prepend_fish_shell_receiver_payloads(
                marker,
                mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                    private_input.receiver_hold.into_bytes(),
                    marker.clone(),
                ),
                mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                    private_input.editor_clear_confirmation.into_bytes(),
                    marker.clone(),
                ),
                mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                    private_input.receiver_admission.into_bytes(),
                    marker.clone(),
                ),
                mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    private_input.receiver_payload.into_bytes(),
                    marker.clone(),
                    private_input.payload_receiver_acknowledgements,
                ),
            );
            private_input.wrapper
        } else if classification == ShellClassification::Zsh {
            let (marker, _) = prepared_bootstrap.as_ref().ok_or_else(|| {
                MezError::invalid_state(
                    "managed Zsh subshell handoff requires a registered bootstrap owner",
                )
            })?;
            let token = zsh_history_token.ok_or_else(|| {
                MezError::invalid_state(
                    "managed Zsh receiver is unavailable for agent subshell handoff",
                )
            })?;
            let trigger = managed_zsh
                .as_ref()
                .map(mez_agent::ManagedZshShell::trigger)
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "managed Zsh trigger is unavailable for agent subshell handoff",
                    )
                })?;
            let private_input =
                mez_agent::zsh_private_source_input(&shell_command, &token, marker, trigger)
                    .map_err(|error| MezError::invalid_state(error.to_string()))?;
            self.prepend_zsh_shell_receiver_payloads(
                marker,
                mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                    private_input.receiver_hold.into_bytes(),
                    marker.clone(),
                ),
                mez_mux::process::ShellInputDelivery::generated_source_for_transaction(
                    private_input.receiver_admission.into_bytes(),
                    marker.clone(),
                ),
                mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                    private_input.receiver_payload.into_bytes(),
                    marker.clone(),
                    private_input.payload_receiver_acknowledgements,
                ),
            );
            private_input.wrapper
        } else {
            shell_command
        };
        match self.write_runtime_pane_shell_input(pane_id, shell_input.as_bytes()) {
            Ok(()) => {
                self.remember_agent_subshell_exit_marker(
                    pane_id,
                    agent_subshell_exit_marker_bytes(&exit_marker),
                );
                if !matches!(
                    classification,
                    ShellClassification::Bash
                        | ShellClassification::Fish
                        | ShellClassification::Zsh
                ) {
                    self.enter_agent_subshell(pane_id);
                    self.take_agent_subshell_command_exit(pane_id);
                    self.remember_hidden_shell_render_suppression(pane_id);
                }
                Ok(true)
            }
            Err(error)
                if error.kind() == MezErrorKind::NotFound
                    || matches!(
                        error.io_kind(),
                        Some(std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected)
                    ) =>
            {
                if prepared_bootstrap.is_some() {
                    self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
                }
                self.clear_agent_subshell_shell_identity(pane_id);
                Ok(false)
            }
            Err(error) => {
                if prepared_bootstrap.is_some() {
                    self.fail_shell_transactions_for_pane_write_failure(pane_id, error.message())?;
                }
                self.clear_agent_subshell_shell_identity(pane_id);
                Err(error)
            }
        }
    }

    /// Leaves the child shell created for agent mode when it is safe to do so.
    ///
    /// If a turn or shell transaction is still active, the subshell remains in
    /// place until the turn finishes so follow-up model actions cannot leak into
    /// the user's parent shell.
    pub(crate) fn exit_agent_subshell_if_active(&mut self, pane_id: &str) -> Result<bool> {
        if !self.agent_subshell_is_active(pane_id) {
            if self.managed_shell_handoff_is_pending(pane_id) {
                return self.request_managed_shell_handoff_exit(pane_id);
            }
            if self.foreign_shell_bootstrap_phase_for_exit(pane_id) == Some("awaiting-prompt") {
                self.clear_uncertified_foreign_shell_boundary(pane_id);
                self.clear_deferred_agent_subshell_entry(pane_id);
                self.invalidate_agent_subshell_environment_after_exit(pane_id);
                self.clear_shell_output_filters_for_foreground_input(pane_id);
                return Ok(true);
            }
            if self
                .cancel_agent_subshell_bootstrap_for_exit(pane_id)
                .is_some()
            {
                self.clear_agent_subshell_state(pane_id);
                self.clear_agent_subshell_shell_identity(pane_id);
                self.clear_shell_output_filters_for_foreground_input(pane_id);
                return Ok(true);
            }
            if self.agent_subshell_input_clear_is_pending(pane_id) {
                self.clear_agent_subshell_state(pane_id);
                self.clear_shell_output_filters_for_foreground_input(pane_id);
                return Ok(true);
            }
            return Ok(false);
        }
        if self
            .agent_shell_store()
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
        {
            return Ok(false);
        }
        if self.managed_shell_handoff_is_pending(pane_id) {
            if self.pane_has_running_shell_transaction(pane_id) {
                return Ok(false);
            }
            return self.request_managed_shell_handoff_exit(pane_id);
        }
        let cancelled_bootstrap_payload = self.cancel_agent_subshell_bootstrap_for_exit(pane_id);
        if self.pane_has_running_shell_transaction(pane_id) {
            return Ok(false);
        }
        if self.primary_pid_for_live_pane_process(pane_id).is_none() {
            self.clear_agent_subshell_state(pane_id);
            self.clear_agent_subshell_shell_identity(pane_id);
            self.clear_shell_output_filters_for_foreground_input(pane_id);
            return Ok(false);
        }
        let retain_input_clear_output = self.agent_subshell_input_clear_was_completed(pane_id);
        if retain_input_clear_output {
            self.remember_hidden_shell_render_suppression(pane_id);
        } else {
            self.clear_shell_output_filters_for_foreground_input(pane_id);
        }
        let managed_shell_handoff_pending = self.managed_shell_handoff_is_pending(pane_id);
        let dependency_free_loader_handoff_pending =
            self.dependency_free_foreign_loader_owns_parent_restoration(pane_id);
        if !managed_shell_handoff_pending && !dependency_free_loader_handoff_pending {
            self.clear_agent_subshell_shell_identity(pane_id);
        }
        let command_exit = self.take_agent_subshell_command_exit(pane_id);
        self.remember_agent_subshell_exit_echo(pane_id);
        let exit_input = if command_exit {
            b"exit\n".as_slice()
        } else {
            b"\x04".as_slice()
        };
        let mut input = cancelled_bootstrap_payload.unwrap_or_default();
        input.extend_from_slice(exit_input);
        match self.write_runtime_pane_input(pane_id, &input) {
            Ok(()) => {
                self.leave_agent_subshell(pane_id);
                self.invalidate_agent_subshell_environment_after_exit(pane_id);
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
                self.clear_agent_subshell_shell_identity(pane_id);
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
        self.exit_agent_subshell_if_active(pane_id)
    }

    /// Runs the persist agent prompt history entry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn persist_agent_prompt_history_entry(
        &mut self,
        pane_id: &str,
        prompt: &ReadlineHistoryEntry,
        queue_for_adapter: bool,
    ) -> Result<()> {
        if prompt.text.trim().is_empty() {
            return Ok(());
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(());
        };
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(());
        };
        if queue_for_adapter {
            let path = store.prompt_history_file(&session.session_id)?;
            self.persistence
                .queue_transcript(RuntimeSideEffect::PersistPromptHistory {
                    path,
                    store,
                    conversation_id: session.session_id.clone(),
                    prompt: prompt.clone(),
                });
            return Ok(());
        }
        let _ = store.append_structured_prompt_history(&session.session_id, prompt)?;
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
        assert_eq!(
            agent_shell_command_plan("/refresh-provider-info"),
            AgentShellCommandPlan::Awaited(AgentShellAwaitedCommand::RefreshProviderInfo)
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
