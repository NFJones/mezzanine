//! Runtime Commands implementation.
//!
//! This module owns the runtime commands boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::agent_state::{
    RuntimeAgentLoopState, RuntimeAgentLoopTurn, RuntimeAgentLoopTurnKind,
    RuntimeAgentRememberDispatch, RuntimeAgentRememberTask,
};
use super::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentContext, AgentId,
    AgentShellCommandOutcome, AgentShellRuntimeContext, AgentShellVisibility, AgentTurnRecord,
    AgentTurnState, BTreeMap, BTreeSet, BlockedApprovalState, ConfigFormat, ConfigMutation,
    ConfigMutationOperation, ConfigMutationValue, ConfigPaths, ConfigScope, ContextBlock,
    ContextSourceKind, DEFAULT_AUTO_SIZING_ROUTER_PROFILE, DeferredAgentPromptHistoryWrite,
    DeferredProjectInstructionWrite, EventKind, HookEvent, MemoryRecord, MemoryScope, MemorySource,
    MezError, ModelProfile, ModelProfileOverrides, PathBuf, RUNTIME_LATENCY_PREFERENCES, Result,
    RuntimeAgentCompactionDispatch, RuntimeAgentCompactionTask, RuntimeAgentPromptTurnStart,
    RuntimeAgentProviderDispatchProvider, RuntimeAgentTurnSteering, RuntimeAgentTurnStop,
    RuntimeAutoSizingConfig, RuntimeModelPreset, RuntimeModelProfileOverrideScope,
    RuntimeSessionService, ScheduledWork, ScheduledWorkKind, SplitDirection, TranscriptEntry,
    TranscriptRole, TrustDecision, agent_shell_visibility_json_name, agent_subshell_enter_command,
    compose_effective_config, current_unix_seconds, discover_project_root,
    execute_agent_shell_command_with_context, execute_command, execute_runtime_command_sequence,
    execute_runtime_command_sequence_async, json_escape, parse_slash_command,
    runtime_add_command_rule, runtime_agent_shell_command_response_json,
    runtime_agent_shell_prompt_turn_response_json, runtime_agent_shell_stop_response_json,
    runtime_agent_turn_state_name, runtime_append_auth_logout_audit,
    runtime_apply_persisted_config_mutation_batch, runtime_approval_command,
    runtime_approval_policy_name, runtime_bypass_approvals_command, runtime_command_outcomes_json,
    runtime_cooperation_mode_name, runtime_effective_config_value,
    runtime_execution_ready_for_provider_continuation, runtime_fit_status_line,
    runtime_list_command_rules_display, runtime_mezzanine_error_code, runtime_model_command_args,
    runtime_model_override_scope_for_args, runtime_model_override_scope_name,
    runtime_model_profile_display, runtime_permission_preset_name, runtime_permissions_command,
    runtime_remove_command_rule, runtime_string_array_json, runtime_user_prompt_hook_payload,
    runtime_validate_latency_preference, runtime_write_agent_context_for_pane,
    runtime_write_agent_copy_output_for_pane, runtime_write_agent_patches_for_pane,
    runtime_write_agent_trace_log_for_pane, select_model_profile, session_state_name,
    shell_command_from_argv, unix_seconds_to_rfc3339,
};
use crate::agent::{
    AgentActionPayload, AllowedActionSet, AsyncModelProvider, DEFAULT_PROVIDER_TIMEOUT_MS,
    ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest, ModelResponse,
    ModelTokenUsage, ModelTokenUsageKey, ProviderApiCompatibility, ProviderCapabilities,
    ProviderModelCatalog, ProviderModelInfo, ProviderQuotaUsage, ReqwestProviderHttpTransport,
    append_mcp_context, deepseek_chat_completions_provider_from_auth_store_with_provider_options,
    effective_provider_api, model_context_text_word_count,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_default_reasoning_levels_for_model,
    openai_responses_provider_from_auth_store_with_provider_options,
};
use crate::auth::AuthCredentialKind;
use crate::error::MezErrorKind;
use crate::readline::ReadlineEdit;
use crate::runtime::config::{
    runtime_default_models_for_provider, runtime_recommended_model_for_provider,
};
use crate::transcript::ConversationSummary;
use base64::Engine;
use std::fs;

mod approval;
mod artifacts;
mod compaction;
mod lists;
mod model;
mod preferences;
mod remember;
mod resume;
mod shell;
mod slash;
mod status;

use approval::*;
#[cfg(test)]
use compaction::*;
pub(super) use model::RuntimeModelCatalog;
use model::*;
use remember::*;
use slash::*;

// Live terminal and agent shell command execution.

/// Conservative per-entry overhead used when estimating transcript replay cost.
const AGENT_COMPACT_TRANSCRIPT_ENTRY_CONTEXT_OVERHEAD_WORDS: usize = 16;
/// Builds the provider-visible prompt for one `/loop` work iteration.
fn runtime_agent_loop_work_prompt(state: &RuntimeAgentLoopState) -> String {
    format!(
        "{}\n\n[loop controller]\nInspect the problem again carefully and fulfill the original user prompt normally. Start from this prompt alone without assuming knowledge of any previous attempt.",
        state.original_prompt
    )
}

impl RuntimeSessionService {
    /// Runs the execute terminal command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn execute_terminal_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &str,
    ) -> Result<String> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let outcomes = execute_runtime_command_sequence(self, primary_client_id, input)?;
        Ok(runtime_command_outcomes_json(&outcomes))
    }

    /// Runs the execute terminal command async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn execute_terminal_command_async(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &str,
    ) -> Result<String> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let outcomes =
            execute_runtime_command_sequence_async(self, primary_client_id, input).await?;
        Ok(runtime_command_outcomes_json(&outcomes))
    }

    /// Toggles the active pane's agent shell and emits the corresponding runtime event.
    /// Executes `/permissions ...` through the runtime permission command path.
    pub(super) fn execute_agent_shell_permissions_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("permissions command must be a slash command"))?;
        let invocation = runtime_single_permissions_invocation(&slash.args)?;
        let body = match invocation.name.as_str() {
            "permissions" => runtime_permissions_command(self, &invocation)?,
            "list-command-rules" => runtime_list_command_rules_display(self.permission_policy()),
            "allow-command" | "deny-command" | "prompt-command" => {
                runtime_add_command_rule(self, &invocation)?
            }
            "remove-command-rule" => runtime_remove_command_rule(self, &invocation)?,
            "bypass-approvals" => runtime_bypass_approvals_command(self, &invocation)?,
            _ => {
                return Err(MezError::invalid_args(format!(
                    "permissions slash command cannot execute {}",
                    invocation.name
                )));
            }
        };
        if body.contains("changed=true") {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "permissions".to_string(),
                body,
                visibility,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "permissions".to_string(),
                body,
            })
        }
    }

    /// Executes `/approval ...` through the runtime approval-mode command path.
    pub(super) fn execute_agent_shell_approval_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("approval command must be a slash command"))?;
        let invocation = runtime_single_approval_invocation(&slash.args)?;
        let body = runtime_approval_command(self, &invocation)?;
        if body.contains("changed=true") {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "approval".to_string(),
                body,
                visibility,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "approval".to_string(),
                body,
            })
        }
    }

    /// Executes `/approve` by deciding a pending pane-local agent approval.
    ///
    /// The command intentionally reuses the `approval/decide` control method so
    /// audit records, persistent shell rules, hooks, and blocked-action resume
    /// behavior stay identical to approval decisions made by external clients.
    pub(super) fn execute_agent_shell_approve_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("approve command must be a slash command"))?;
        let args = slash.args.trim();
        let (selection, action_kind, action_summary) = {
            let pending = self
                .blocked_approvals
                .pending()
                .into_iter()
                .filter(|approval| approval.pane_id == pane_id)
                .collect::<Vec<_>>();
            if matches!(args, "list" | "pending") {
                return Ok(AgentShellCommandOutcome::Display {
                    command: "approve".to_string(),
                    body: agent_approve_pending_display(pane_id, &pending),
                });
            }
            let selection = parse_agent_approve_selection(args, pane_id, &pending)?;
            let approval = self
                .blocked_approvals
                .get(&selection.approval_id)
                .filter(|approval| {
                    approval.pane_id == pane_id && approval.state == BlockedApprovalState::Pending
                })
                .ok_or_else(|| {
                    MezError::invalid_args(format!(
                        "pending approval {} was not found for pane {pane_id}",
                        selection.approval_id
                    ))
                })?;
            (
                selection,
                approval.action_kind.clone(),
                approval.action_summary.clone(),
            )
        };
        let idempotency_key = format!(
            "agent-approve-{}-{}",
            selection.approval_id,
            current_unix_seconds()
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":"agent-approve","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"{}"}},"idempotency_key":"{}"}}}}"#,
            json_escape(&selection.approval_id),
            selection.scope.as_str(),
            json_escape(&idempotency_key)
        );
        let response = self.dispatch_runtime_control_body(&request, primary_client_id);
        if let Some(message) = agent_approve_control_error_message(&response) {
            return Err(MezError::invalid_args(message));
        }
        Ok(AgentShellCommandOutcome::Mutated {
            command: "approve".to_string(),
            body: format!(
                "approval {} approved scope={} action={} summary={}",
                selection.approval_id,
                selection.scope.as_str(),
                action_kind,
                agent_approval_summary_preview(&action_summary)
            ),
            visibility,
        })
    }

    /// Executes `/trust` by trusting a pending project overlay root.
    ///
    /// Trust decisions reuse the runtime `project/trust/decide` path so the
    /// trust database, config-layer reload, lifecycle events, and audit records
    /// stay identical to decisions made through the control API.
    pub(super) fn execute_agent_shell_trust_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let visibility = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        let slash = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("trust command must be a slash command"))?;
        let args = slash.args.trim();
        let pending = self.pending_project_trust_requests_for_agent_work();
        if matches!(args, "list" | "pending") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "trust".to_string(),
                body: agent_project_trust_pending_display(&pending),
            });
        }
        let selection = agent_select_project_trust_request(args, &pending)?;
        let root = selection.project_root;
        let root_text = root.to_string_lossy().to_string();
        let idempotency_key = format!(
            "agent-trust-{}-{}",
            current_unix_seconds(),
            agent_approval_summary_preview(&root_text)
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":"agent-trust","method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"{}"}}}}"#,
            json_escape(&root_text),
            json_escape(&idempotency_key)
        );
        let response = self.dispatch_runtime_control_body(&request, primary_client_id);
        if let Some(message) = agent_approve_control_error_message(&response) {
            return Err(MezError::invalid_args(message));
        }
        let persistence_path = self
            .project_trust_database_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "unconfigured".to_string());
        Ok(AgentShellCommandOutcome::Mutated {
            command: "trust".to_string(),
            body: format!(
                "project trust granted root={} persisted={} persistence_path={} overlays={}",
                agent_path_preview(&root),
                self.project_trust_database_path.is_some(),
                agent_approval_summary_preview(&persistence_path),
                selection.overlay_files.len()
            ),
            visibility,
        })
    }

    /// Runs the stop agent turn for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop_agent_turn_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<RuntimeAgentTurnStop> {
        let turn_id = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.clone())
            .or_else(|| {
                self.agent_scheduler
                    .queued_turns()
                    .find(|work| work.pane_id.as_deref() == Some(pane_id))
                    .map(|work| work.turn_id.clone())
            })
            .ok_or_else(|| MezError::invalid_state("agent shell session has no running turn"))?;
        let scheduler_cancelled = self.agent_scheduler.cancel(&turn_id).is_ok();
        let interrupted_shell_transactions =
            self.cancel_live_shell_transactions_for_turn(&turn_id)?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        self.emit_cancelled_subagent_task_result(&turn)?;
        let session = if self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            == Some(turn_id.as_str())
        {
            self.finish_agent_turn(pane_id, &turn_id, AgentTurnState::Interrupted)?
        } else {
            self.finish_agent_turn_without_shell_session(&turn, AgentTurnState::Interrupted)?
        };
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"cancelled","interrupted_shell_transactions":{}}}"#,
                json_escape(pane_id),
                json_escape(&turn_id),
                interrupted_shell_transactions
            ),
        )?;
        Ok(RuntimeAgentTurnStop {
            turn_id,
            scheduler_cancelled,
            interrupted_shell_transactions,
            visibility: session.visibility,
        })
    }

    /// Runs the cancel live shell transactions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn cancel_live_shell_transactions_for_turn(
        &mut self,
        turn_id: &str,
    ) -> Result<usize> {
        let cancelled = self
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.turn_id == turn_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.pane_id.clone()))
            .collect::<Vec<_>>();
        if cancelled.is_empty() {
            return Ok(0);
        }

        let mut interrupted_panes = BTreeSet::new();
        for (marker, pane_id) in &cancelled {
            self.running_shell_transactions.remove(marker);
            self.clear_shell_transaction_protocol_state(marker);
            if interrupted_panes.insert(pane_id.clone()) {
                if self.agent_subshell_panes.contains(pane_id) {
                    self.agent_subshell_command_exit_panes
                        .insert(pane_id.clone());
                }
                match self.write_runtime_pane_input(pane_id, b"\x03") {
                    Ok(_) => {}
                    Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }

        Ok(cancelled.len())
    }

    /// Runs the apply agent shell preference context operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_agent_shell_preference_context(
        &self,
        pane_id: &str,
        mut context: AgentContext,
    ) -> Result<AgentContext> {
        if let Some(prompt) = self.custom_agent_system_prompt.as_ref() {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::System,
                label: "configured agent system prompt".to_string(),
                content: prompt.clone(),
            });
        }
        if let Some(profile) = self.agent_selected_personality_profile(pane_id)
            && let Some(prompt) = profile.system_prompt.as_ref()
        {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::System,
                label: "agent personality system prompt".to_string(),
                content: prompt.clone(),
            });
        }
        if let Some(directive) = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.directive.as_deref())
        {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::DeveloperInstruction,
                label: "agent shell directive".to_string(),
                content: format!(
                    "Pane-local /directive instruction for this session. Append it to the existing developer instructions for future turns:\n{}",
                    directive
                ),
            });
        }
        let selected_profile = self.agent_selected_personality_profile(pane_id);
        let planning_enabled = self.agent_planning_modes.contains(pane_id)
            || selected_profile.is_some_and(|profile| profile.planning_enabled == Some(true));
        if planning_enabled {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::Configuration,
                label: "agent shell plan mode".to_string(),
                content: "Planning mode is active. For broad or ambiguous work, briefly state the execution approach before acting. Do not use a visible plan when the next safe inspection, edit, validation, or repair action is clear."
                    .to_string(),
            });
        }
        let profile_style = selected_profile.and_then(|profile| profile.response_style.as_deref());
        if let Some(style) = self
            .agent_response_styles
            .get(pane_id)
            .map(String::as_str)
            .or(profile_style)
        {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::Configuration,
                label: "agent shell personality".to_string(),
                content: format!("Response style preference for this pane: {style}."),
            });
        }
        AgentContext::new(context.blocks)
    }

    /// Runs the start agent prompt turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_agent_prompt_turn(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<RuntimeAgentPromptTurnStart> {
        self.start_agent_prompt_turn_inner(pane_id, prompt, None)
    }

    /// Starts a `/loop` command by creating the first loop-owned work turn.
    pub(super) fn execute_agent_shell_loop_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("loop command must be submitted as an agent slash command")
        })?;
        let original_prompt = invocation.args.trim();
        if original_prompt.is_empty() {
            return Err(MezError::invalid_args("/loop requires a non-empty prompt"));
        }
        if self.agent_loops_by_pane.contains_key(pane_id) {
            return Err(MezError::conflict(
                "an agent loop is already active for this pane",
            ));
        }
        self.append_agent_user_prompt_to_terminal_buffer(pane_id, input)?;
        let parent_session = self.agent_shell_store.get(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent shell session not found for pane",
            )
        })?;
        let parent_conversation_id = parent_session.session_id.clone();
        let parent_prompt_cache_lineage_id = parent_session.prompt_cache_lineage_id.clone();
        self.agent_loops_by_pane.insert(
            pane_id.to_string(),
            RuntimeAgentLoopState {
                pane_id: pane_id.to_string(),
                original_prompt: original_prompt.to_string(),
                parent_conversation_id: parent_conversation_id.clone(),
                parent_prompt_cache_lineage_id: Some(parent_prompt_cache_lineage_id),
                iteration: 1,
                emitted_apply_patch: false,
                max_iterations: self.agent_loop_limit.max(1),
            },
        );
        let started = match self.start_agent_loop_work_turn(pane_id) {
            Ok(started) => started,
            Err(error) => {
                self.agent_loops_by_pane.remove(pane_id);
                return Err(error);
            }
        };
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "loop".to_string(),
            body: format!(
                "pane={} agent_prompt_turn={} loop_iteration=1 loop_limit={} parent_conversation={} state={}",
                pane_id,
                started.turn_id,
                self.agent_loop_limit.max(1),
                parent_conversation_id,
                runtime_agent_turn_state_name(started.state)
            ),
            visibility,
        })
    }

    /// Starts one loop-owned work turn using the current pane loop state.
    pub(in crate::runtime) fn start_agent_loop_work_turn(
        &mut self,
        pane_id: &str,
    ) -> Result<RuntimeAgentPromptTurnStart> {
        let state = self
            .agent_loops_by_pane
            .get(pane_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("agent loop state is unavailable"))?;
        let store = self
            .agent_transcript_store
            .clone()
            .ok_or_else(|| MezError::invalid_state("agent transcript store is unavailable"))?;
        let target_conversation_id = Self::runtime_new_agent_conversation_id();
        let summary = match store.fork(
            &state.parent_conversation_id,
            &target_conversation_id,
            current_unix_seconds().max(1),
        ) {
            Ok(summary) => summary,
            Err(error)
                if (error.kind() == crate::error::MezErrorKind::InvalidState
                    && error.message() == "source conversation has no entries")
                    || (error.kind() == crate::error::MezErrorKind::NotFound
                        && error.message() == "conversation transcript not found") =>
            {
                crate::transcript::ConversationSummary {
                    conversation_id: target_conversation_id,
                    entries: 0,
                    first_created_at_unix_seconds: 0,
                    last_created_at_unix_seconds: 0,
                    last_turn_id: String::new(),
                    agent_id: String::new(),
                    pane_id: pane_id.to_string(),
                    directory: self
                        .pane_current_working_directory(pane_id)
                        .map(|path| path.to_string_lossy().into_owned()),
                    initial_prompt: None,
                    latest_user_prompt: None,
                }
            }
            Err(error) => return Err(error),
        };
        let (session_id, transcript_entries) = {
            let session = self.agent_shell_store.bind_conversation_with_lineage(
                pane_id,
                &summary.conversation_id,
                summary.entries as u64,
                state.parent_prompt_cache_lineage_id.clone(),
            )?;
            (session.session_id.clone(), session.transcript_entries)
        };
        let prompt = runtime_agent_loop_work_prompt(&state);
        let started = self.start_agent_prompt_turn_inner(pane_id, &prompt, None)?;
        self.agent_loop_turns.insert(
            started.turn_id.clone(),
            RuntimeAgentLoopTurn {
                pane_id: pane_id.to_string(),
                kind: RuntimeAgentLoopTurnKind::Work,
                iteration: state.iteration,
            },
        );
        self.append_agent_trace_turn_event(
            pane_id,
            &started.turn_id,
            &format!(
                "loop work queued iteration={} limit={} parent_conversation={} conversation_id={} entries={}",
                state.iteration,
                state.max_iterations,
                state.parent_conversation_id,
                session_id,
                transcript_entries
            ),
        )?;
        Ok(started)
    }

    /// Injects user steering input into the currently running pane turn.
    ///
    /// Provider requests already in flight cannot be edited, so the text is
    /// retained as pending steering and drained into the next provider-bound
    /// context. The visible agent prompt has already logged the submitted user
    /// text before this helper runs.
    pub(super) fn inject_agent_steering_for_running_turn(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<Option<String>> {
        let Some(turn_id) = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .map(str::to_string)
        else {
            return Ok(None);
        };
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| {
                turn.turn_id == turn_id
                    && turn.pane_id == pane_id
                    && turn.state == AgentTurnState::Running
            })
            .cloned()
        else {
            return Ok(None);
        };
        self.agent_turn_pending_steering
            .entry(turn.turn_id.clone())
            .or_default()
            .push(RuntimeAgentTurnSteering {
                input: input.to_string(),
                submitted_at_unix_seconds: current_unix_seconds(),
            });
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: queued steering input for current turn {}; it will be incorporated after the next step",
                turn.turn_id
            ),
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn.turn_id,
            "user_steering queued reason=mid_turn_agent_prompt",
        )?;
        if !self.pending_agent_provider_tasks.contains(&turn.turn_id)
            && !self
                .claimed_agent_provider_tasks
                .contains_key(&turn.turn_id)
            && self
                .agent_turn_executions
                .get(&turn.turn_id)
                .is_some_and(runtime_execution_ready_for_provider_continuation)
        {
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
            self.append_agent_trace_turn_event(
                pane_id,
                &turn.turn_id,
                "provider_task queued reason=user_steering_ready_for_provider_continuation",
            )?;
        }
        Ok(Some(turn.turn_id))
    }

    /// Runs the start agent prompt turn with cooperation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_agent_prompt_turn_with_cooperation(
        &mut self,
        pane_id: &str,
        prompt: &str,
        cooperation_mode: Option<String>,
    ) -> Result<RuntimeAgentPromptTurnStart> {
        self.start_agent_prompt_turn_inner(pane_id, prompt, cooperation_mode)
    }

    /// Runs the start agent prompt turn inner operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn start_agent_prompt_turn_inner(
        &mut self,
        pane_id: &str,
        prompt: &str,
        cooperation_mode: Option<String>,
    ) -> Result<RuntimeAgentPromptTurnStart> {
        self.refresh_project_config_layers_for_pane(pane_id)?;
        if let Some(project_trust_request) = self
            .pending_project_trust_requests_for_agent_work()
            .into_iter()
            .next()
        {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &agent_project_trust_log_line(&project_trust_request),
            )?;
            return Err(MezError::conflict(format!(
                "project trust decision pending for {}",
                project_trust_request.project_root.display()
            )));
        }
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::UserPromptSubmit,
            &runtime_user_prompt_hook_payload(pane_id, prompt),
        )? {
            return Err(MezError::forbidden(format!(
                "user prompt blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        let context = self.agent_context_for_pane_prompt(pane_id, prompt, 100)?;
        let context = self.apply_agent_shell_preference_context(pane_id, context)?;
        let turn_id = self.next_agent_turn_id();
        let agent_id = format!("agent-{pane_id}");
        let context_blocks = context.blocks.len();
        let created_at_unix_seconds = current_unix_seconds();
        let prompt_preview = prompt.chars().take(160).collect::<String>();
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let turn = AgentTurnRecord {
            turn_id: turn_id.clone(),
            agent_id: agent_id.clone(),
            pane_id: pane_id.to_string(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: created_at_unix_seconds,
            policy_profile: "runtime".to_string(),
            model_profile: model_profile_name.clone(),
            parent_turn_id: None,
            cooperation_mode: cooperation_mode.clone(),
            state: AgentTurnState::Queued,
        };
        self.agent_turn_ledger.queue_turn(turn.clone())?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            "created state=queued reason=user_prompt_submitted",
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            &format!(
                "context prepared blocks={} model_profile={}",
                context_blocks, model_profile_name
            ),
        )?;
        self.agent_turn_contexts.insert(turn_id.clone(), context);
        self.agent_turn_model_profiles
            .insert(turn_id.clone(), model_profile);
        self.agent_scheduler.enqueue(ScheduledWork {
            turn_id: turn_id.clone(),
            agent_id: agent_id.clone(),
            pane_id: Some(pane_id.to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })?;
        self.append_agent_trace_turn_event(
            pane_id,
            &turn_id,
            "scheduler enqueue kind=shell_capable",
        )?;
        self.start_ready_agent_turns_suppressing_status_for(Some(&turn_id))?;
        let state = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state)
            .ok_or_else(|| MezError::invalid_state("queued agent turn disappeared"))?;
        match state {
            AgentTurnState::Queued => {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    "agent: queued and waiting for a turn slot",
                )?;
            }
            AgentTurnState::Running => {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    "agent: working on the request",
                )?;
            }
            AgentTurnState::Blocked
            | AgentTurnState::Completed
            | AgentTurnState::Failed
            | AgentTurnState::Interrupted => {}
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","model_profile":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&turn_id),
                runtime_agent_turn_state_name(state),
                json_escape(&model_profile_name)
            ),
        )?;
        Ok(RuntimeAgentPromptTurnStart {
            turn_id,
            agent_id,
            state,
            created_at_unix_seconds,
            started_at_unix_seconds: matches!(state, AgentTurnState::Running)
                .then_some(created_at_unix_seconds),
            finished_at_unix_seconds: None,
            prompt_preview,
            approval_ids: Vec::new(),
            result_summary: None,
            context_blocks,
        })
    }

    /// Runs the pending project trust requests for agent work operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pending_project_trust_requests_for_agent_work(&self) -> Vec<AgentProjectTrustRequest> {
        let mut requests: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for layer in &self.config_layers {
            if layer.scope != ConfigScope::ProjectOverlay || layer.trusted {
                continue;
            }
            let root = layer
                .path
                .as_ref()
                .and_then(|path| path.parent().map(discover_project_root))
                .or_else(|| layer.path.as_ref().map(|path| discover_project_root(path)))
                .unwrap_or_else(|| PathBuf::from("."));
            let decision = self
                .project_trust_store
                .as_ref()
                .and_then(|store| store.get(&root))
                .map(|record| record.state)
                .unwrap_or(TrustDecision::Pending);
            if decision == TrustDecision::Pending {
                if let Some(path) = layer.path.as_ref() {
                    requests.entry(root).or_default().push(path.clone());
                } else {
                    requests.entry(root).or_default();
                }
            }
        }
        requests
            .into_iter()
            .map(|(project_root, mut overlay_files)| {
                overlay_files.sort();
                overlay_files.dedup();
                AgentProjectTrustRequest {
                    project_root,
                    overlay_files,
                }
            })
            .collect()
    }

    /// Runs the next agent turn id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn next_agent_turn_id(&self) -> String {
        let mut index = self.agent_turn_ledger.turns().len().saturating_add(1);
        loop {
            let candidate = format!("turn-{index}");
            if !self
                .agent_turn_ledger
                .turns()
                .iter()
                .any(|turn| turn.turn_id == candidate)
            {
                return candidate;
            }
            index = index.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentAction, MaapBatch};

    /// Verifies model compaction consumes the structured `say` action text
    /// returned by the provider. This keeps manual and automatic compaction
    /// tied to the model-authored summary rather than accidentally storing the
    /// provider's raw MAAP envelope as durable memory.
    #[test]
    fn runtime_model_compaction_summary_prefers_say_action_text() {
        let response = ModelResponse {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            raw_text: "raw envelope".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "compact-as1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![AgentAction {
                    id: "summary".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "## Summary\n\nKeep this.".to_string(),
                        content_type: "text/markdown; charset=utf-8".to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        };

        let summary = runtime_model_compaction_summary_from_response(&response).unwrap();

        assert_eq!(summary, "## Summary\n\nKeep this.");
    }

    /// Verifies that restricted API-key model-list failures are collapsed to a
    /// stable user-facing reason. This keeps `/model list` readable when the
    /// provider rejects `/v1/models` because the credential is missing the
    /// `api.model.read` scope.
    #[test]
    fn runtime_model_catalog_unavailable_reason_names_missing_model_read_scope() {
        let reason = runtime_model_catalog_unavailable_reason(
            "You have insufficient permissions for this operation. Missing scopes: api.model.read.",
        );

        assert_eq!(reason, "missing-model-read-scope");
    }

    /// Verifies the model compaction prompt exposes only the `say` action
    /// surface and includes bounded transcript source material. Compaction
    /// should be a summarization request, not a normal tool-capable agent turn.
    #[test]
    fn runtime_model_compaction_request_is_say_only() {
        let profile = ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: BTreeMap::new(),
            safety_tier: None,
        };
        let entries = vec![TranscriptEntry {
            conversation_id: "as1".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            content: "Need a compact summary".to_string(),
        }];

        let context = AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            label: "user prompt".to_string(),
            content: "Need a compact summary".to_string(),
        }])
        .unwrap();
        let request =
            runtime_model_compaction_request(&profile, "%1", "as1", 1, &entries, &context).unwrap();

        assert_eq!(
            request.interaction_kind,
            ModelInteractionKind::ActionExecution
        );
        assert_eq!(request.allowed_actions.actions.len(), 2);
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.content.contains("Need a compact summary")),
            "{request:?}"
        );
    }

    /// Verifies the retained compaction tail is based on a percentage of model
    /// context budget rather than a fixed entry count. This protects terse
    /// follow-up prompts whose referenced list is recent in bytes but older
    /// than the previous fixed eight-entry replay tail.
    #[test]
    fn runtime_compact_retained_transcript_entries_uses_context_budget_tail() {
        let entries = runtime_compact_test_entries(12, 230);

        let retained = runtime_compact_retained_transcript_entries(12, &entries, 20_000, 10);

        assert_eq!(retained, 8);
    }

    /// Verifies the retained compaction tail percentage is configurable.
    ///
    /// Operators may need a larger exact recent suffix when their workflows rely
    /// on terse follow-up references, so the helper must derive retention from
    /// the configured percentage instead of a fixed built-in value.
    #[test]
    fn runtime_compact_retained_transcript_entries_honors_configured_tail_percent() {
        let entries = runtime_compact_test_entries(12, 230);

        let default_retained =
            runtime_compact_retained_transcript_entries(12, &entries, 10_000, 10);
        let larger_retained = runtime_compact_retained_transcript_entries(12, &entries, 10_000, 20);

        assert!(larger_retained > default_retained);
    }

    /// Verifies forced compaction keeps normal retention when possible, but
    /// shrinks an all-covering raw tail enough to leave summary input.
    #[test]
    fn runtime_compact_forced_retained_entries_leaves_summary_input() {
        let entries = runtime_compact_test_entries(12, 230);

        let retained = runtime_compact_forced_retained_transcript_entries(12, &entries, 20_000, 10);
        let summarized = runtime_compact_transcript_entries_for_summary(12, &entries, retained);

        assert_eq!(retained, 8);
        assert_eq!(summarized.first().map(|entry| entry.sequence), Some(1));

        let single_entry = runtime_compact_test_entries(1, 230);
        let retained =
            runtime_compact_forced_retained_transcript_entries(1, &single_entry, 20_000, 10);
        let summarized = runtime_compact_transcript_entries_for_summary(1, &single_entry, retained);

        assert_eq!(retained, 0);
        assert_eq!(summarized.first().map(|entry| entry.sequence), Some(1));
    }

    /// Verifies compaction summarizes only the active transcript prefix outside
    /// the retained raw tail. The retained suffix remains available verbatim to
    /// later model turns and is not duplicated into the compacted summary input.
    #[test]
    fn runtime_compact_entries_for_summary_excludes_retained_tail() {
        let entries = runtime_compact_test_entries(12, 230);

        let retained = runtime_compact_retained_transcript_entries(12, &entries, 10_000, 10);
        let summarized = runtime_compact_transcript_entries_for_summary(12, &entries, retained);

        assert!(retained > 0, "{retained}");
        assert!(retained < 12, "{retained}");
        assert_eq!(summarized.first().map(|entry| entry.sequence), Some(1));
        assert_eq!(
            summarized.last().map(|entry| entry.sequence),
            Some(12 - retained)
        );
    }

    /// Builds deterministic transcript entries for compaction helper tests.
    ///
    /// # Parameters
    /// - `count`: The number of entries to build.
    /// - `content_words`: The content words stored in each entry.
    fn runtime_compact_test_entries(count: u64, content_words: usize) -> Vec<TranscriptEntry> {
        (1..=count)
            .map(|sequence| TranscriptEntry {
                conversation_id: "compact-test".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: if sequence % 2 == 0 {
                    TranscriptRole::User
                } else {
                    TranscriptRole::Assistant
                },
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: "context-word ".repeat(content_words),
            })
            .collect()
    }

    /// Verifies `/loop` work prompts remain identical across iterations so the
    /// model restarts from the original task instead of inheriting prior loop
    /// attempt context.
    #[test]
    fn runtime_agent_loop_work_prompt_stays_fresh_across_iterations() {
        let first = runtime_agent_loop_work_prompt(&RuntimeAgentLoopState {
            pane_id: "%1".to_string(),
            original_prompt: "review this document".to_string(),
            parent_conversation_id: "parent-conversation".to_string(),
            parent_prompt_cache_lineage_id: Some("lineage-1".to_string()),
            iteration: 1,
            emitted_apply_patch: false,
            max_iterations: 8,
        });
        let later = runtime_agent_loop_work_prompt(&RuntimeAgentLoopState {
            pane_id: "%1".to_string(),
            original_prompt: "review this document".to_string(),
            parent_conversation_id: "parent-conversation".to_string(),
            parent_prompt_cache_lineage_id: Some("lineage-1".to_string()),
            iteration: 3,
            emitted_apply_patch: false,
            max_iterations: 8,
        });

        assert_eq!(first, later);
        assert!(first.contains("review this document"), "{first}");
        assert!(first.contains("Start from this prompt alone"), "{first}");
        assert!(!first.contains("work iteration 3/8"), "{first}");
        assert!(!first.contains("Previous loop assessment"), "{first}");
    }
}
