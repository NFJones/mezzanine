//! Runtime Commands implementation.
//!
//! This module owns the runtime commands boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentContext,
    AgentShellCommandOutcome, AgentShellRuntimeContext, AgentShellVisibility, AgentTurnRecord,
    AgentTurnState, BTreeMap, BTreeSet, BlockedApprovalRequest, BlockedApprovalState, Command,
    CommandInvocation, ConfigFormat, ConfigScope, ContextBlock, ContextSourceKind,
    DEFAULT_AUTO_SIZING_ROUTER_PROFILE, DeferredAgentPromptHistoryWrite,
    DeferredProjectInstructionWrite, EventKind, HookEvent, MemoryRecord, MemoryScope, MemorySource,
    MezError, ModelProfile, ModelProfileOverrides, Path, PathBuf, RUNTIME_LATENCY_PREFERENCES,
    Result, RuntimeAgentCompactionDispatch, RuntimeAgentCompactionTask,
    RuntimeAgentPromptTurnStart, RuntimeAgentProviderDispatchProvider, RuntimeAgentTurnSteering,
    RuntimeAgentTurnStop, RuntimeAutoSizingConfig, RuntimeModelPreset,
    RuntimeModelProfileOverrideScope, RuntimeSessionService, ScheduledWork, ScheduledWorkKind,
    SplitDirection, TranscriptEntry, TranscriptRole, TrustDecision, Value,
    agent_shell_visibility_json_name, agent_subshell_enter_command, compose_effective_config,
    current_unix_seconds, discover_project_root, execute_agent_shell_command_with_context,
    execute_command, execute_runtime_command_sequence, execute_runtime_command_sequence_async,
    json_escape, parse_command_sequence, parse_slash_command, runtime_add_command_rule,
    runtime_agent_shell_command_response_json, runtime_agent_shell_prompt_turn_response_json,
    runtime_agent_shell_stop_response_json, runtime_agent_turn_state_name,
    runtime_append_auth_logout_audit, runtime_approval_command, runtime_approval_policy_name,
    runtime_bypass_approvals_command, runtime_command_outcomes_json, runtime_cooperation_mode_name,
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
    ModelTokenUsage, ModelTokenUsageKey, ProviderCapabilities, ProviderModelCatalog,
    ProviderModelInfo, ProviderQuotaUsage, ReqwestProviderHttpTransport, append_mcp_context,
    model_context_text_word_count, openai_default_reasoning_levels_for_model,
    openai_provider_from_auth_store_with_provider_options,
};
use crate::auth::AuthCredentialKind;
use crate::error::MezErrorKind;
use crate::readline::ReadlineEdit;
use crate::runtime::config::{
    runtime_default_models_for_provider, runtime_recommended_model_for_provider,
};
use base64::Engine;

mod compaction;
mod model;

#[cfg(test)]
use compaction::*;
pub(super) use model::RuntimeModelCatalog;
use model::*;

// Live terminal and agent shell command execution.

/// Result of applying the live side effects for an agent-shell exit request.
pub(super) struct RuntimeAgentShellExit {
    /// Conversation id associated with the pane-local agent shell.
    conversation_id: String,
    /// Visibility after the exit request and any required stop operation.
    visibility: AgentShellVisibility,
    /// Turn id stopped before hiding, when exit interrupted active work.
    stopped_turn_id: Option<String>,
}

/// Conservative per-entry overhead used when estimating transcript replay cost.
const AGENT_COMPACT_TRANSCRIPT_ENTRY_CONTEXT_OVERHEAD_WORDS: usize = 16;

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

/// Returns the saved working directory from transcript context entries.
///
/// # Parameters
/// - `entries`: The durable transcript entries for one conversation.
fn runtime_resume_directory_from_entries(entries: &[TranscriptEntry]) -> Option<String> {
    let mut project_root = None;
    for entry in entries {
        for line in entry.content.lines() {
            if let Some(value) = line
                .strip_prefix("cwd=")
                .or_else(|| line.strip_prefix("working_directory="))
                && !value.trim().is_empty()
            {
                return Some(value.trim().to_string());
            }
            if project_root.is_none()
                && let Some(value) = line.strip_prefix("project_root=")
                && !value.trim().is_empty()
            {
                project_root = Some(value.trim().to_string());
            }
        }
    }
    project_root
}

/// Formats saved system transcript metadata for human resume replay.
///
/// # Parameters
/// - `content`: The saved system transcript entry body.
fn runtime_resume_system_display_content(content: &str) -> String {
    let entry = TranscriptEntry {
        conversation_id: "resume-display".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "resume-display".to_string(),
        agent_id: "agent-resume-display".to_string(),
        pane_id: "%resume-display".to_string(),
        content: content.to_string(),
    };
    runtime_resume_directory_from_entries(&[entry])
        .map(|directory| format!("Session directory: {directory}"))
        .unwrap_or_else(|| content.to_string())
}

/// Carries Agent Approve Scope state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentApproveScope {
    /// Represents the Once case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Once,
    /// Represents the Session case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Session,
    /// Represents the Project case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Project,
    /// Represents the Global case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Global,
}

impl AgentApproveScope {
    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn parse(value: &str) -> Option<Self> {
        match value {
            "once" => Some(Self::Once),
            "session" => Some(Self::Session),
            "project" => Some(Self::Project),
            "global" => Some(Self::Global),
            _ => None,
        }
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

/// Carries Agent Approve Selection state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentApproveSelection {
    /// Stores the approval id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    approval_id: String,
    /// Stores the scope value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    scope: AgentApproveScope,
}

/// Carries Agent Project Trust Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentProjectTrustRequest {
    /// Stores the project root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    project_root: PathBuf,
    /// Stores the overlay files value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    overlay_files: Vec<PathBuf>,
}

/// Runs the parse agent approve selection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_agent_approve_selection(
    args: &str,
    pane_id: &str,
    pending_for_pane: &[&BlockedApprovalRequest],
) -> Result<AgentApproveSelection> {
    let tokens = args.split_whitespace().collect::<Vec<_>>();
    match tokens.as_slice() {
        [] => Ok(AgentApproveSelection {
            approval_id: select_default_agent_approval_id(pane_id, pending_for_pane)?,
            scope: AgentApproveScope::Once,
        }),
        [scope] if AgentApproveScope::parse(scope).is_some() => Ok(AgentApproveSelection {
            approval_id: select_default_agent_approval_id(pane_id, pending_for_pane)?,
            scope: AgentApproveScope::parse(scope)
                .ok_or_else(|| MezError::invalid_args("invalid approval scope"))?,
        }),
        [approval_id] => Ok(AgentApproveSelection {
            approval_id: select_named_agent_approval_id(approval_id, pane_id, pending_for_pane)?,
            scope: AgentApproveScope::Once,
        }),
        [approval_id, scope] => Ok(AgentApproveSelection {
            approval_id: select_named_agent_approval_id(approval_id, pane_id, pending_for_pane)?,
            scope: AgentApproveScope::parse(scope).ok_or_else(|| {
                MezError::invalid_args("/approve scope must be once, session, project, or global")
            })?,
        }),
        _ => Err(MezError::invalid_args(
            "/approve expects [approval-id|latest] [once|session|project|global]",
        )),
    }
}

/// Runs the select default agent approval id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn select_default_agent_approval_id(
    pane_id: &str,
    pending_for_pane: &[&BlockedApprovalRequest],
) -> Result<String> {
    match pending_for_pane {
        [] => Err(MezError::invalid_args(format!(
            "no pending approvals for pane {pane_id}"
        ))),
        [approval] => Ok(approval.id.clone()),
        _ => Err(MezError::invalid_args(format!(
            "multiple pending approvals for pane {pane_id}; use /approve <approval-id>\n{}",
            pending_agent_approval_lines(pending_for_pane).join("\n")
        ))),
    }
}

/// Runs the select named agent approval id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn select_named_agent_approval_id(
    requested: &str,
    pane_id: &str,
    pending_for_pane: &[&BlockedApprovalRequest],
) -> Result<String> {
    if requested == "latest" {
        return pending_for_pane
            .last()
            .map(|approval| approval.id.clone())
            .ok_or_else(|| {
                MezError::invalid_args(format!("no pending approvals for pane {pane_id}"))
            });
    }
    Ok(requested.to_string())
}

/// Runs the pending agent approval lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pending_agent_approval_lines(pending: &[&BlockedApprovalRequest]) -> Vec<String> {
    pending
        .iter()
        .map(|approval| {
            format!(
                "approval {} pending: {} {}",
                approval.id,
                approval.action_kind,
                agent_approval_summary_preview(&approval.action_summary)
            )
        })
        .collect()
}

/// Runs the agent approval summary preview operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_approval_summary_preview(summary: &str) -> String {
    /// Defines the MAX APPROVAL SUMMARY CHARS const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_APPROVAL_SUMMARY_CHARS: usize = 160;
    let mut preview = String::new();
    let mut chars = summary.trim().chars();
    for _ in 0..MAX_APPROVAL_SUMMARY_CHARS {
        let Some(ch) = chars.next() else {
            return preview;
        };
        preview.push(match ch {
            '\r' | '\n' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        });
    }
    if chars.next().is_some() {
        preview.push_str("...");
    }
    preview
}

/// Runs the agent approve control error message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_approve_control_error_message(response: &str) -> Option<String> {
    serde_json::from_str::<Value>(response)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// Runs the agent approve pending display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_approve_pending_display(pane_id: &str, pending: &[&BlockedApprovalRequest]) -> String {
    if pending.is_empty() {
        format!("no pending approvals for pane {pane_id}")
    } else {
        format!(
            "pending approvals for pane {pane_id}:\n{}\nUse /approve <approval-id> [once|session|project|global].",
            pending_agent_approval_lines(pending).join("\n")
        )
    }
}

/// Runs the agent project trust log line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_project_trust_log_line(request: &AgentProjectTrustRequest) -> String {
    format!(
        "project trust pending: {} overlays={} (trust with /trust {})",
        agent_path_preview(&request.project_root),
        request.overlay_files.len(),
        agent_path_preview(&request.project_root)
    )
}

/// Runs the agent project trust pending display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_project_trust_pending_display(pending: &[AgentProjectTrustRequest]) -> String {
    if pending.is_empty() {
        "no pending project trust requests".to_string()
    } else {
        format!(
            "pending project trust requests:\n{}\nUse /trust <project-root>.",
            pending
                .iter()
                .map(agent_project_trust_log_line)
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

/// Runs the agent select project trust request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_select_project_trust_request(
    args: &str,
    pending: &[AgentProjectTrustRequest],
) -> Result<AgentProjectTrustRequest> {
    let args = args.trim();
    match args {
        "" => match pending {
            [] => Err(MezError::invalid_args("no pending project trust requests")),
            [request] => Ok(request.clone()),
            _ => Err(MezError::invalid_args(format!(
                "multiple pending project trust requests; use /trust <project-root>\n{}",
                agent_project_trust_pending_display(pending)
            ))),
        },
        "latest" => pending
            .last()
            .cloned()
            .ok_or_else(|| MezError::invalid_args("no pending project trust requests")),
        "list" | "pending" => Err(MezError::invalid_state(
            "project trust list requests must be handled before selection",
        )),
        path => {
            let requested_root = discover_project_root(&PathBuf::from(path));
            pending
                .iter()
                .find(|request| project_trust_root_matches(&request.project_root, &requested_root))
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_args(format!(
                        "pending project trust request {} was not found",
                        requested_root.display()
                    ))
                })
        }
    }
}

/// Runs the project trust root matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn project_trust_root_matches(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Runs the agent path preview operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn agent_path_preview(path: &Path) -> String {
    agent_approval_summary_preview(&path.to_string_lossy())
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
    pub(super) fn toggle_active_agent_shell(
        &mut self,
    ) -> Result<(String, String, AgentShellVisibility)> {
        let pane_id = self.active_pane_id()?;
        let visible = self
            .agent_shell_store
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
    pub(super) fn request_agent_shell_exit_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<RuntimeAgentShellExit> {
        let parent_agent_id = format!("agent-{pane_id}");
        self.close_subagent_descendants_for_parent_agent(
            &parent_agent_id,
            "parent agent shell exited",
        )?;
        let conversation_id = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .ok_or_else(|| MezError::invalid_state("agent shell session not found for pane"))?;
        let running_turn_id = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.clone());
        if running_turn_id.is_some() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: stopping active turn before exiting agent shell; pane input is blocked until stop completes",
            )?;
            self.agent_shell_store
                .request_hide_pending_task_completion(pane_id)?;
            let stopped = self.stop_agent_turn_for_pane(pane_id)?;
            return Ok(RuntimeAgentShellExit {
                conversation_id,
                visibility: stopped.visibility,
                stopped_turn_id: Some(stopped.turn_id),
            });
        }

        let session = self.agent_shell_store.request_exit(pane_id)?;
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
    pub(super) fn enter_agent_mode_for_pane(&mut self, pane_id: &str) -> Result<String> {
        let conversation_id = self
            .agent_shell_store
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
        primary_client_id: &crate::ids::ClientId,
        input: &str,
    ) -> Result<String> {
        self.execute_agent_shell_command_with_display(primary_client_id, input, input)
    }

    /// Executes an agent prompt submission while allowing a collapsed display
    /// form for pane transcript rendering.
    pub(super) fn execute_agent_shell_command_with_display(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &str,
        display_input: &str,
    ) -> Result<String> {
        self.execute_agent_shell_command_with_display_inner(primary_client_id, input, display_input)
    }

    /// Executes an agent prompt submission while allowing a collapsed display
    /// form for pane transcript rendering.
    fn execute_agent_shell_command_with_display_inner(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &str,
        display_input: &str,
    ) -> Result<String> {
        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let pane_id = self.active_pane_id()?;
        let visible = self
            .agent_shell_store
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
        self.persist_agent_prompt_history_entry(&pane_id, input)?;
        if is_prompt {
            self.append_agent_user_prompt_to_terminal_buffer(&pane_id, display_input)?;
        }
        let outcome = match execute_agent_shell_command_with_context(
            &mut self.agent_shell_store,
            &pane_id,
            input,
            AgentShellRuntimeContext {
                mcp_registry: Some(&self.mcp_registry),
                permission_policy: Some(&self.permission_policy),
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
                    let init_outcome = self.execute_agent_shell_init_command(&pane_id, input)?;
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
                    && command == "statusline"
                {
                    let statusline_outcome =
                        self.execute_agent_shell_statusline_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&statusline_outcome),
                    )
                } else if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                    outcome.as_ref()
                    && command == "title"
                {
                    let title_outcome =
                        self.execute_agent_shell_title_command(primary_client_id, &pane_id, input)?;
                    runtime_agent_shell_command_response_json(&pane_id, input, Some(&title_outcome))
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
            self.agent_modified_files.remove(&pane_id);
            self.reload_agent_prompt_history_for_pane(&pane_id)?;
        }
        if exit_requires_runtime
            && self
                .agent_shell_store
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
        let needs_discovery = self.mcp_registry.list_servers().into_iter().any(|server| {
            server.configured.enabled && server.status == crate::mcp::McpServerStatus::Configured
        });
        if !needs_discovery {
            return Ok(());
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(MezError::invalid_state(
                "synchronous /list-mcp discovery cannot run inside an active async runtime",
            ));
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

    /// Runs the execute agent shell command async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn execute_agent_shell_command_async(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        input: &str,
    ) -> Result<String> {
        let slash_invocation = parse_slash_command(input).ok().flatten();
        let is_model_command = slash_invocation
            .as_ref()
            .is_some_and(|invocation| invocation.name == "model");
        let is_compact_command = slash_invocation
            .as_ref()
            .is_some_and(|invocation| invocation.name == "compact");
        let is_list_mcp_command = slash_invocation
            .as_ref()
            .is_some_and(|invocation| invocation.name == "list-mcp");
        let is_prompt = !input.trim().is_empty() && slash_invocation.is_none();
        if !is_model_command && !is_compact_command && !is_list_mcp_command && !is_prompt {
            return self.execute_agent_shell_command(primary_client_id, input);
        }

        if is_prompt {
            self.require_live()?;
            if self.session.primary_client_id() != Some(primary_client_id) {
                return Err(MezError::forbidden("operation requires the primary client"));
            }
            let pane_id = self.active_pane_id()?;
            let visible = self
                .agent_shell_store
                .get(&pane_id)
                .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
            if !visible {
                return Err(MezError::invalid_state(
                    "agent shell prompt requires a visible agent shell session",
                ));
            }
            return self.execute_agent_shell_command_with_display_inner(
                primary_client_id,
                input,
                input,
            );
        }

        self.require_live()?;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(MezError::forbidden("operation requires the primary client"));
        }
        let pane_id = self.active_pane_id()?;
        let visible = self
            .agent_shell_store
            .get(&pane_id)
            .is_some_and(|session| session.visibility == AgentShellVisibility::Visible);
        if !visible {
            return Err(MezError::invalid_state(
                "agent shell prompt requires a visible agent shell session",
            ));
        }
        if is_list_mcp_command {
            let _ = self
                .ensure_runtime_mcp_transports_discovered_async()
                .await?;
        }
        self.persist_agent_prompt_history_entry(&pane_id, input)?;
        let outcome = match execute_agent_shell_command_with_context(
            &mut self.agent_shell_store,
            &pane_id,
            input,
            AgentShellRuntimeContext {
                mcp_registry: Some(&self.mcp_registry),
                permission_policy: Some(&self.permission_policy),
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
            if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                outcome.as_ref()
                && command == "model"
            {
                let model_outcome = self
                    .execute_agent_shell_model_command_async(&pane_id, input)
                    .await?;
                return Ok(runtime_agent_shell_command_response_json(
                    &pane_id,
                    input,
                    Some(&model_outcome),
                ));
            }
            if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                outcome.as_ref()
                && command == "latency"
            {
                let latency_outcome = self.execute_agent_shell_latency_command(&pane_id, input)?;
                return Ok(runtime_agent_shell_command_response_json(
                    &pane_id,
                    input,
                    Some(&latency_outcome),
                ));
            }
            if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                outcome.as_ref()
                && command == "thinking"
            {
                let thinking_outcome =
                    self.execute_agent_shell_thinking_command(&pane_id, input)?;
                return Ok(runtime_agent_shell_command_response_json(
                    &pane_id,
                    input,
                    Some(&thinking_outcome),
                ));
            }
            if let Some(AgentShellCommandOutcome::RequiresRuntime { command, .. }) =
                outcome.as_ref()
                && command == "compact"
            {
                let compact_outcome = self
                    .execute_agent_shell_compact_command_async(&pane_id, input)
                    .await?;
                return Ok(runtime_agent_shell_command_response_json(
                    &pane_id,
                    input,
                    Some(&compact_outcome),
                ));
            }
            Ok(runtime_agent_shell_command_response_json(
                &pane_id,
                input,
                outcome.as_ref(),
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
    pub(super) fn enter_agent_subshell_if_needed(&mut self, pane_id: &str) -> Result<bool> {
        if self.agent_subshell_panes.contains(pane_id)
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
                self.agent_subshell_panes.insert(pane_id.to_string());
                self.agent_subshell_command_exit_panes.remove(pane_id);
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
    pub(super) fn exit_agent_subshell_if_active(&mut self, pane_id: &str) -> Result<bool> {
        if !self.agent_subshell_panes.contains(pane_id) {
            return Ok(false);
        }
        if self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
            || self
                .running_shell_transactions
                .values()
                .any(|transaction| transaction.pane_id == pane_id)
        {
            return Ok(false);
        }
        if self.primary_pid_for_live_pane_process(pane_id).is_none() {
            self.agent_subshell_panes.remove(pane_id);
            self.agent_subshell_command_exit_panes.remove(pane_id);
            self.clear_shell_output_filters_for_foreground_input(pane_id);
            return Ok(false);
        }
        self.clear_shell_output_filters_for_foreground_input(pane_id);
        let input = if self.agent_subshell_command_exit_panes.remove(pane_id) {
            b"exit\n".as_slice()
        } else {
            b"\x04".as_slice()
        };
        match self.write_runtime_pane_input(pane_id, input) {
            Ok(()) => {
                self.agent_subshell_panes.remove(pane_id);
                Ok(true)
            }
            Err(error)
                if error.kind() == MezErrorKind::NotFound
                    || matches!(
                        error.io_kind(),
                        Some(std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected)
                    ) =>
            {
                self.agent_subshell_panes.remove(pane_id);
                self.agent_subshell_command_exit_panes.remove(pane_id);
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
    pub(super) fn advance_pane_shell_prompt_after_agent_exit(
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
    fn persist_agent_prompt_history_entry(&mut self, pane_id: &str, input: &str) -> Result<()> {
        if input.trim().is_empty() {
            return Ok(());
        }
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(());
        };
        let Some(session) = self.agent_shell_store.get(pane_id) else {
            return Ok(());
        };
        if self.defer_agent_transcript_writes {
            self.deferred_agent_prompt_history_writes
                .push(DeferredAgentPromptHistoryWrite {
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

    /// Executes `/routing` against pane-scoped auto-sizing state.
    pub(super) fn execute_agent_shell_routing_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("routing command must be a slash command"))?;
        let mode = runtime_single_mode_arg(&invocation.args, "routing", "toggle")?;
        let default_enabled = self.agent_routing;
        let enabled_before = self
            .agent_routing_overrides
            .get(pane_id)
            .copied()
            .unwrap_or(default_enabled);
        if matches!(mode.as_str(), "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "routing".to_string(),
                body: format!(
                    "pane={} enabled={} default={} override_present={} source=runtime-routing",
                    json_escape(pane_id),
                    enabled_before,
                    default_enabled,
                    self.agent_routing_overrides.contains_key(pane_id)
                ),
            });
        }
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        let enabled = match mode.as_str() {
            "on" => true,
            "off" => false,
            "toggle" => !enabled_before,
            _ => {
                return Err(MezError::invalid_args(
                    "routing slash command expects on, off, toggle, status, or no argument",
                ));
            }
        };
        self.agent_routing_overrides
            .insert(pane_id.to_string(), enabled);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "routing".to_string(),
            body: format!(
                "pane={} enabled={} default={} changed={} source=runtime-routing",
                json_escape(pane_id),
                enabled,
                default_enabled,
                enabled != enabled_before
            ),
            visibility,
        })
    }

    /// Executes `/personality` against pane-scoped response style state.
    pub(super) fn execute_agent_shell_personality_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("personality command must be a slash command"))?;
        let requested = invocation.args.trim();
        let current = self.agent_response_styles.get(pane_id).cloned();
        let current_profile = self
            .agent_selected_personality_profile_id(pane_id)
            .map(ToOwned::to_owned);
        if requested.is_empty() || matches!(requested, "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "personality".to_string(),
                body: format!(
                    "pane={} profile={} style={} configured_profiles={} source=runtime-personality",
                    json_escape(pane_id),
                    current_profile
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "default".to_string()),
                    current
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "default".to_string()),
                    self.agent_personality_profiles.len()
                ),
            });
        }
        if requested == "list" {
            let profiles = self
                .agent_personality_profiles
                .iter()
                .map(|(id, profile)| {
                    format!(
                        "{}{}",
                        id,
                        profile
                            .name
                            .as_deref()
                            .map(|name| format!(" ({name})"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>();
            return Ok(AgentShellCommandOutcome::Display {
                command: "personality".to_string(),
                body: format!(
                    "profiles=[{}] default={} source=runtime-personality",
                    profiles.join(", "),
                    self.default_agent_personality
                        .as_deref()
                        .map(json_escape)
                        .unwrap_or_else(|| "none".to_string())
                ),
            });
        }
        validate_agent_personality(requested)?;
        let visibility = self.agent_shell_visibility_for_pane(pane_id)?;
        if matches!(requested, "clear" | "default") {
            let changed =
                current.is_some() || self.agent_personality_selections.contains_key(pane_id);
            self.agent_personality_selections.remove(pane_id);
            self.agent_response_styles.remove(pane_id);
            return Ok(AgentShellCommandOutcome::Mutated {
                command: "personality".to_string(),
                body: format!(
                    "pane={} profile=default style=default changed={} source=runtime-personality",
                    json_escape(pane_id),
                    changed
                ),
                visibility,
            });
        }
        let requested_style =
            if let Some(profile) = self.agent_personality_profiles.get(requested).cloned() {
                self.agent_personality_selections
                    .insert(pane_id.to_string(), requested.to_string());
                self.apply_agent_personality_profile_overrides(pane_id, &profile)?;
                profile.response_style
            } else {
                self.agent_personality_selections.remove(pane_id);
                Some(requested.to_string())
            };
        let changed = current != requested_style || current_profile.as_deref() != Some(requested);
        if let Some(style) = requested_style {
            self.agent_response_styles
                .insert(pane_id.to_string(), style);
        } else {
            self.agent_response_styles.remove(pane_id);
        }
        let active = self.agent_response_styles.get(pane_id);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "personality".to_string(),
            body: format!(
                "pane={} profile={} style={} changed={} source=runtime-personality",
                json_escape(pane_id),
                self.agent_selected_personality_profile_id(pane_id)
                    .map(json_escape)
                    .unwrap_or_else(|| "custom".to_string()),
                active
                    .map(|style| json_escape(style))
                    .unwrap_or_else(|| "default".to_string()),
                changed
            ),
            visibility,
        })
    }

    /// Applies runtime overrides supplied by a configured personality profile.
    ///
    /// # Parameters
    /// - `pane_id`: The pane receiving the profile overrides.
    /// - `profile`: The configured profile selected by `/personality`.
    fn apply_agent_personality_profile_overrides(
        &mut self,
        pane_id: &str,
        profile: &super::RuntimeAgentPersonalityProfile,
    ) -> Result<()> {
        if let Some(model_profile) = profile.model_profile.as_ref() {
            if self.provider_registry.profile(model_profile).is_none() {
                return Err(MezError::invalid_args(format!(
                    "personality model_profile `{model_profile}` is not configured"
                )));
            }
            self.model_profile_overrides
                .pane_profiles
                .insert(pane_id.to_string(), model_profile.clone());
        }
        if let Some(planning_enabled) = profile.planning_enabled {
            if planning_enabled {
                self.agent_planning_modes.insert(pane_id.to_string());
            } else {
                self.agent_planning_modes.remove(pane_id);
            }
        }
        if let Some(routing_enabled) = profile.routing_enabled {
            self.agent_routing_overrides
                .insert(pane_id.to_string(), routing_enabled);
        }
        Ok(())
    }

    /// Returns the selected or default personality profile id for a pane.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose selected profile should be resolved.
    pub(super) fn agent_selected_personality_profile_id(&self, pane_id: &str) -> Option<&str> {
        self.agent_personality_selections
            .get(pane_id)
            .map(String::as_str)
            .or(self.default_agent_personality.as_deref())
            .filter(|profile_id| self.agent_personality_profiles.contains_key(*profile_id))
    }

    /// Returns the selected or default personality profile for a pane.
    ///
    /// # Parameters
    /// - `pane_id`: The pane whose selected profile should be resolved.
    pub(super) fn agent_selected_personality_profile(
        &self,
        pane_id: &str,
    ) -> Option<&super::RuntimeAgentPersonalityProfile> {
        self.agent_selected_personality_profile_id(pane_id)
            .and_then(|profile_id| self.agent_personality_profiles.get(profile_id))
    }

    /// Runs the agent shell visibility for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_shell_visibility_for_pane(
        &self,
        pane_id: &str,
    ) -> Result<AgentShellVisibility> {
        self.agent_shell_store
            .get(pane_id)
            .map(|session| session.visibility)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })
    }

    /// Runs the execute agent shell resume command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_resume_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("resume command must be a slash command"))?;
        let conversation_arg = invocation.args.split_whitespace().next();
        if conversation_arg.is_none() {
            return self.execute_agent_shell_list_sessions_command(pane_id);
        }
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "resume".to_string(),
                body: "conversations=0 source=unavailable".to_string(),
            });
        };
        let conversation_id = match conversation_arg {
            Some("--latest" | "latest") => {
                let summaries = store.list()?;
                let Some(conversation_id) = Self::runtime_latest_agent_saved_session_id(&summaries)
                else {
                    return Ok(AgentShellCommandOutcome::Display {
                        command: "resume".to_string(),
                        body: "conversations=0 source=runtime-resume latest=false reason=no-saved-sessions"
                            .to_string(),
                    });
                };
                conversation_id
            }
            Some(conversation_id) => conversation_id.to_string(),
            None => unreachable!("bare resume returns through list-sessions before store lookup"),
        };
        let entries = store.inspect(&conversation_id)?;
        let presentation_entries = store.inspect_presentation(&conversation_id)?;
        let resume_directory = runtime_resume_directory_from_entries(&entries);
        let (session_id, transcript_entries, visibility) = {
            let session = self.agent_shell_store.bind_conversation(
                pane_id,
                &conversation_id,
                entries.len() as u64,
            )?;
            (
                session.session_id.clone(),
                session.transcript_entries,
                session.visibility,
            )
        };
        self.restore_agent_resume_directory(pane_id, resume_directory.as_deref())?;
        self.restore_agent_resume_state_for_conversation(pane_id, &session_id)?;
        self.record_pane_transcript_ref(pane_id, format!("transcript:{pane_id}:{session_id}"))?;
        self.reload_agent_prompt_history_for_pane(pane_id)?;
        self.clear_agent_shell_terminal_view(pane_id)?;
        if !self
            .replay_agent_presentation_entries_to_terminal_buffer(pane_id, &presentation_entries)?
        {
            self.set_agent_prompt_display_lines(
                pane_id,
                Self::runtime_resume_transcript_display(&entries),
            )?;
        }
        Ok(AgentShellCommandOutcome::Mutated {
            command: "resume".to_string(),
            body: format!(
                "conversation_id={} entries={} pane={} resumed=true",
                session_id, transcript_entries, pane_id
            ),
            visibility,
        })
    }

    /// Returns the latest saved agent session using the same ordering as the
    /// saved-session picker.
    ///
    /// # Parameters
    /// - `summaries`: The saved conversation summaries to sort.
    fn runtime_latest_agent_saved_session_id(
        summaries: &[crate::transcript::ConversationSummary],
    ) -> Option<String> {
        let mut sorted_summaries = summaries.iter().collect::<Vec<_>>();
        sorted_summaries.sort_by(|left, right| {
            right
                .last_created_at_unix_seconds
                .cmp(&left.last_created_at_unix_seconds)
                .then_with(|| {
                    right
                        .first_created_at_unix_seconds
                        .cmp(&left.first_created_at_unix_seconds)
                })
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        sorted_summaries
            .first()
            .map(|summary| summary.conversation_id.clone())
    }

    /// Restores the pane to a saved session directory when that directory is
    /// still available.
    ///
    /// # Parameters
    /// - `pane_id`: The pane being rebound to the saved conversation.
    /// - `resume_directory`: The directory persisted with the saved session.
    fn restore_agent_resume_directory(
        &mut self,
        pane_id: &str,
        resume_directory: Option<&str>,
    ) -> Result<()> {
        let Some(resume_directory) = resume_directory.filter(|value| !value.trim().is_empty())
        else {
            return Ok(());
        };
        let path = PathBuf::from(resume_directory);
        if !path.is_dir() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!(
                    "agent: resume directory unavailable; staying in current directory: {}",
                    runtime_fit_status_line(resume_directory, 160)
                ),
            )?;
            return Ok(());
        }
        self.pane_current_working_directories
            .insert(pane_id.to_string(), path.clone());
        if self.primary_pid_for_live_pane_process(pane_id).is_some() {
            let mut command =
                shell_command_from_argv(&["cd".to_string(), path.to_string_lossy().into_owned()])?;
            command.push('\n');
            if let Err(error) = self.write_runtime_pane_input(pane_id, command.as_bytes()) {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!(
                        "agent: resume directory recorded but shell cd failed: {}",
                        runtime_fit_status_line(error.message(), 160)
                    ),
                )?;
            }
        }
        Ok(())
    }

    /// Runs the execute agent shell fork command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_fork_command(
        &mut self,
        primary_client_id: &crate::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("fork command must be a slash command"))?;
        let source = self
            .agent_shell_store
            .get(pane_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?
            .session_id
            .clone();
        let source_descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "source pane not found",
            )
        })?;
        let source_start_directory = self.pane_current_working_directory(pane_id);
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "fork".to_string(),
                body: format!(
                    "current_conversation={} forked=false reason=transcript-store-unavailable source=runtime-fork",
                    json_escape(&source)
                ),
            });
        };
        let prompt_seed =
            Self::runtime_agent_fork_prompt_seed(&store.prompt_history(&source)?, input);
        let source_lineage = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.prompt_cache_lineage_id.clone());
        let target = invocation
            .args
            .split_whitespace()
            .next()
            .map(ToOwned::to_owned)
            .unwrap_or_else(Self::runtime_new_agent_conversation_id);
        let summary = store.fork(&source, &target, current_unix_seconds().max(1))?;
        let started = self.split_pane_in_window_with_process(
            primary_client_id,
            &source_descriptor.window_id,
            SplitDirection::Vertical,
            true,
            None,
            source_start_directory.as_deref(),
        )?;
        self.agent_shell_store.enter_or_resume(&started.pane_id)?;
        let (session_id, transcript_entries, visibility) = {
            let session = self.agent_shell_store.bind_conversation_with_lineage(
                &started.pane_id,
                &summary.conversation_id,
                summary.entries as u64,
                source_lineage,
            )?;
            (
                session.session_id.clone(),
                session.transcript_entries,
                session.visibility,
            )
        };
        self.record_pane_transcript_ref(
            &started.pane_id,
            format!("transcript:{}:{session_id}", started.pane_id),
        )?;
        self.enter_agent_mode_for_pane(&started.pane_id)?;
        if let Some(seed) = prompt_seed
            && let Some(prompt_input) = self.agent_prompt_inputs.get_mut(&started.pane_id)
        {
            prompt_input
                .prompt
                .buffer
                .apply(ReadlineEdit::InsertText(seed));
        }
        Ok(AgentShellCommandOutcome::Mutated {
            command: "fork".to_string(),
            body: format!(
                "source={} conversation_id={} entries={} source_pane={} pane={} forked=true",
                source, session_id, transcript_entries, pane_id, started.pane_id
            ),
            visibility,
        })
    }

    /// Returns the prompt text that should seed a newly forked agent pane.
    ///
    /// # Parameters
    /// - `history`: Shared persisted agent prompt history for the source
    ///   conversation.
    /// - `current_input`: The `/fork` command currently being executed.
    fn runtime_agent_fork_prompt_seed(history: &[String], current_input: &str) -> Option<String> {
        let current = current_input.trim();
        history
            .iter()
            .rev()
            .find(|entry| {
                let trimmed = entry.trim();
                !trimmed.is_empty() && (current.is_empty() || trimmed != current)
            })
            .cloned()
    }

    /// Returns a version-four UUID string for a newly forked conversation.
    fn runtime_new_agent_conversation_id() -> String {
        let mut bytes: [u8; 16] = rand::random();
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            bytes[0],
            bytes[1],
            bytes[2],
            bytes[3],
            bytes[4],
            bytes[5],
            bytes[6],
            bytes[7],
            bytes[8],
            bytes[9],
            bytes[10],
            bytes[11],
            bytes[12],
            bytes[13],
            bytes[14],
            bytes[15]
        )
    }

    /// Executes `/list-sessions` and returns markdown for the prompt renderer.
    ///
    /// The attached prompt path owns pane-buffer rendering for display outcomes.
    /// Keeping this command side-effect-free prevents duplicate command output
    /// when the response body is rendered as markdown.
    fn execute_agent_shell_list_sessions_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let body = self.runtime_agent_list_sessions_display(pane_id)?;
        Ok(AgentShellCommandOutcome::Display {
            command: "list-sessions".to_string(),
            body,
        })
    }

    /// Executes `/list-skills` and returns the effective skill catalog.
    ///
    /// The command is read-only and intentionally uses the same effective
    /// catalog as `$skill` prompt expansion so users see only skills that can
    /// be selected explicitly in the current pane.
    fn execute_agent_shell_list_skills_command(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        self.refresh_project_config_layers_for_pane(pane_id)?;
        Ok(AgentShellCommandOutcome::Display {
            command: "list-skills".to_string(),
            body: self.runtime_agent_skill_catalog_display(pane_id),
        })
    }

    /// Builds the user-facing skill catalog display for `/list-skills`.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose config root and trusted project root determine
    ///   the effective skill set.
    fn runtime_agent_skill_catalog_display(&self, pane_id: &str) -> String {
        let catalog = self.effective_skill_catalog_for_pane(pane_id);
        let mut lines = vec![
            "## Skills".to_string(),
            String::new(),
            "Start a prompt with `$` and press Tab to select a skill by name.".to_string(),
            "Submit `$<skill-name> [additional context]` to invoke a skill explicitly.".to_string(),
            String::new(),
        ];
        if catalog.skills.is_empty() {
            lines.push("No skills are currently available.".to_string());
        } else {
            lines.push(format!("{} skills available:", catalog.skills.len()));
            lines.push(String::new());
            let rows = catalog
                .skills
                .iter()
                .map(|skill| {
                    vec![
                        format!("`${}`", skill.name),
                        skill.source.as_str().to_string(),
                        skill.description.clone(),
                    ]
                })
                .collect::<Vec<_>>();
            lines.extend(runtime_markdown_table(
                &["Skill", "Scope", "Description"],
                &rows,
            ));
        }
        if !catalog.diagnostics.is_empty() {
            lines.push(String::new());
            lines.push("Skipped skill diagnostics:".to_string());
            lines.extend(catalog.diagnostics.iter().map(|diagnostic| {
                format!("- `{}` - {}", diagnostic.path.display(), diagnostic.message)
            }));
        }
        lines.join("\n")
    }

    /// Executes `/list-modified-files` and returns a compact markdown list.
    fn execute_agent_shell_list_modified_files_command(
        &self,
        pane_id: &str,
    ) -> AgentShellCommandOutcome {
        AgentShellCommandOutcome::Display {
            command: "list-modified-files".to_string(),
            body: self.runtime_agent_modified_files_display(pane_id),
        }
    }

    /// Builds the pane-local modified-file summary used by the agent shell.
    fn runtime_agent_modified_files_display(&self, pane_id: &str) -> String {
        let Some(files) = self.agent_modified_files.get(pane_id) else {
            return "## modified files\n\nno modified files tracked for this agent conversation."
                .to_string();
        };
        if files.is_empty() {
            return "## modified files\n\nno modified files tracked for this agent conversation."
                .to_string();
        }
        let total_added = files.values().map(|summary| summary.added).sum::<usize>();
        let total_removed = files.values().map(|summary| summary.removed).sum::<usize>();
        let mut lines = vec![
            "## modified files".to_string(),
            String::new(),
            format!(
                "{} ({} {}, {} files)",
                "summary",
                Self::markdown_modified_file_count_span("mez-diff-addition", '+', total_added),
                Self::markdown_modified_file_count_span("mez-diff-deletion", '-', total_removed),
                files.len()
            ),
            String::new(),
        ];
        for summary in files.values() {
            lines.push(format!(
                "- edited `{}` ({} {})",
                summary.path,
                Self::markdown_modified_file_count_span("mez-diff-addition", '+', summary.added),
                Self::markdown_modified_file_count_span("mez-diff-deletion", '-', summary.removed)
            ));
        }
        lines.join("\n")
    }

    /// Wraps one modified-file line count in a markdown span consumed by the
    /// terminal markdown renderer.
    ///
    /// # Parameters
    /// - `class_name`: The renderer-recognized presentation class.
    /// - `sign`: The leading `+` or `-` count sign.
    /// - `count`: The count to render.
    fn markdown_modified_file_count_span(class_name: &str, sign: char, count: usize) -> String {
        format!(r#"<span class="{class_name}">{sign}{count}</span>"#)
    }

    /// Builds `/list-sessions` from saved agent conversation transcripts.
    fn runtime_agent_list_sessions_display(&self, pane_id: &str) -> Result<String> {
        let width = self
            .pane_screens
            .get(pane_id)
            .map(|screen| usize::from(screen.size().columns))
            .unwrap_or(120);
        if let Some(store) = self.agent_transcript_store.as_ref() {
            return Ok(Self::runtime_agent_saved_sessions_display(
                &store.list()?,
                width,
            ));
        }
        Ok(self.runtime_current_session_display())
    }

    /// Formats the active in-memory session for `/list-sessions` fallback output.
    fn runtime_current_session_display(&self) -> String {
        let attached_clients = self
            .session
            .clients()
            .iter()
            .filter(|client| client.state == crate::session::ClientState::Attached)
            .count();
        let last_attached_at = self
            .session
            .last_attached_at_unix_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_else(|| "none".to_string());
        let mut lines = vec![
            "## Agent Sessions".to_string(),
            String::new(),
            "No saved agent transcript store is configured.".to_string(),
            String::new(),
            "### Live Mezzanine Session".to_string(),
            String::new(),
        ];
        let rows = vec![vec![
            self.session.id.to_string(),
            self.session.name.clone(),
            session_state_name(self.session.state).to_string(),
            unix_seconds_to_rfc3339(self.session.created_at_unix_seconds),
            last_attached_at,
            self.session.windows().len().to_string(),
            self.session.clients().len().to_string(),
            attached_clients.to_string(),
            self.session.primary_client_id().is_none().to_string(),
        ]];
        lines.extend(runtime_markdown_table(
            &[
                "Session",
                "Name",
                "State",
                "Created",
                "Last attached",
                "Windows",
                "Clients",
                "Attached clients",
                "Primary available",
            ],
            &rows,
        ));
        lines.join("\n")
    }

    /// Encodes one agent command as a markdown link destination.
    fn markdown_link_destination(command: &str) -> String {
        let mut encoded = String::from("mez-agent:");
        for byte in command.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    encoded.push(char::from(byte))
                }
                other => encoded.push_str(&format!("%{other:02X}")),
            }
        }
        encoded
    }

    /// Formats saved agent conversations as a nested resume list.
    fn runtime_agent_saved_sessions_display(
        summaries: &[crate::transcript::ConversationSummary],
        width: usize,
    ) -> String {
        let mut lines = vec!["## Agent Sessions".to_string(), String::new()];
        if summaries.is_empty() {
            lines.push("No saved agent sessions are available.".to_string());
            return lines.join("\n");
        }
        let mut sorted_summaries = summaries.iter().collect::<Vec<_>>();
        sorted_summaries.sort_by(|left, right| {
            right
                .last_created_at_unix_seconds
                .cmp(&left.last_created_at_unix_seconds)
                .then_with(|| {
                    right
                        .first_created_at_unix_seconds
                        .cmp(&left.first_created_at_unix_seconds)
                })
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        for (index, summary) in sorted_summaries.iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            let resume_command = format!("/resume {}", summary.conversation_id);
            lines.push(format!(
                "- [**{}**]({})",
                summary.conversation_id,
                Self::markdown_link_destination(&resume_command)
            ));
            lines.push(format!(
                "  - Last Active: {}",
                unix_seconds_to_rfc3339(summary.last_created_at_unix_seconds)
            ));
            lines.push(format!(
                "  - Directory: {}",
                summary.directory.as_deref().unwrap_or("-")
            ));
            lines.push(format!(
                "  - Prompt: {}",
                runtime_fit_status_line(
                    summary.latest_user_prompt.as_deref().unwrap_or("-"),
                    width.saturating_sub("  - Prompt: ".len())
                )
            ));
            lines.push("  - Resume: select the linked session id above.".to_string());
        }
        lines.join("\n")
    }

    /// Formats a resumed transcript as prompt display lines so the user can
    /// pick up the saved conversation with visible context in the pane.
    fn runtime_resume_transcript_display(entries: &[TranscriptEntry]) -> Vec<String> {
        let mut lines = vec!["Resumed Agent Session".to_string()];
        if entries.is_empty() {
            lines.push("No saved transcript entries were found.".to_string());
            return lines;
        }
        if let Some(first) = entries.first() {
            lines.push(format!(
                "Conversation ID: {} | Entries: {} | Resumed: yes",
                json_escape(&first.conversation_id),
                entries.len()
            ));
        }
        lines.push(String::new());
        for entry in entries {
            let content = Self::runtime_resume_entry_display_content(entry);
            if content.trim().is_empty() {
                continue;
            }
            let prefix = match entry.role {
                TranscriptRole::User => "user> ",
                TranscriptRole::Assistant => "mez> ",
                TranscriptRole::Tool => "agent: ",
                TranscriptRole::System => "system> ",
            };
            lines.push(format!(
                "{}{}",
                prefix,
                Self::runtime_resume_entry_preview(&content)
            ));
        }
        lines
    }

    /// Builds user-visible content for one resumed transcript entry.
    fn runtime_resume_entry_display_content(entry: &TranscriptEntry) -> String {
        match entry.role {
            TranscriptRole::Tool => Self::runtime_resume_tool_display_content(&entry.content),
            TranscriptRole::System => runtime_resume_system_display_content(&entry.content),
            TranscriptRole::User | TranscriptRole::Assistant => {
                Self::runtime_resume_best_effort_text(&entry.content)
            }
        }
    }

    /// Extracts the human-facing text from stored tool transcript content.
    fn runtime_resume_tool_display_content(content: &str) -> String {
        let text = Self::runtime_resume_best_effort_text(content);
        if let Some(extracted) = Self::runtime_resume_structured_text(&text) {
            return extracted;
        }
        if let Some(extracted) = Self::runtime_resume_content_field_text(&text) {
            return extracted;
        }
        text
    }

    /// Decodes accidental base64 transcript content when it is clearly text.
    fn runtime_resume_best_effort_text(content: &str) -> String {
        let trimmed = content.trim();
        Self::runtime_resume_base64_text(trimmed).unwrap_or_else(|| content.to_string())
    }

    /// Decodes one strict base64 text payload for transcript replay.
    fn runtime_resume_base64_text(content: &str) -> Option<String> {
        if content.len() < 8 || !content.len().is_multiple_of(4) {
            return None;
        }
        if !content
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        {
            return None;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(content.as_bytes())
            .ok()?;
        let text = String::from_utf8(decoded).ok()?;
        if text.is_empty()
            || !text
                .chars()
                .all(|ch| matches!(ch, '\n' | '\r' | '\t') || !ch.is_control())
        {
            return None;
        }
        Some(text)
    }

    /// Extracts `structured_content.text` from replayed tool content.
    fn runtime_resume_structured_text(content: &str) -> Option<String> {
        for marker in ["structured_content: ", "structured_content="] {
            let Some((_before, after)) = content.split_once(marker) else {
                continue;
            };
            let value = serde_json::from_str::<serde_json::Value>(after.trim()).ok()?;
            if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                return Some(text.to_string());
            }
            if let Some(text) = value
                .get("structured_content")
                .and_then(|structured| structured.get("text"))
                .and_then(serde_json::Value::as_str)
            {
                return Some(text.to_string());
            }
        }
        None
    }

    /// Extracts a plain `content:` field from replayed tool content.
    fn runtime_resume_content_field_text(content: &str) -> Option<String> {
        let (_before, after) = content.split_once("content: ")?;
        let value = after
            .split(" structured_content:")
            .next()
            .unwrap_or(after)
            .trim();
        (!value.is_empty()).then(|| value.to_string())
    }

    /// Builds one bounded single-line transcript preview.
    fn runtime_resume_entry_preview(content: &str) -> String {
        let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.chars().count() <= 160 {
            return normalized;
        }
        let mut preview = normalized.chars().take(159).collect::<String>();
        preview.push('…');
        preview
    }

    /// Executes `/copy-context` against the active pane's model request context.
    fn execute_agent_shell_copy_context_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("copy-context command must be a slash command")
        })?;
        let (body, mutated) =
            runtime_write_agent_context_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy-context".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy-context".to_string(),
                body,
            })
        }
    }

    /// Executes `/copy-trace-log` against the retained bounded pane trace log.
    fn execute_agent_shell_copy_trace_log_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("copy-trace-log command must be a slash command")
        })?;
        let (body, mutated) =
            runtime_write_agent_trace_log_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy-trace-log".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy-trace-log".to_string(),
                body,
            })
        }
    }

    /// Executes `/copy-patches` against retained patch payloads for this session.
    fn execute_agent_shell_copy_patches_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("copy-patches command must be a slash command")
        })?;
        let (body, mutated) =
            runtime_write_agent_patches_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy-patches".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy-patches".to_string(),
                body,
            })
        }
    }

    /// Executes `/diff` against the pane's current version-control context.
    pub(super) fn execute_agent_shell_diff_command(
        &self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        Ok(AgentShellCommandOutcome::Display {
            command: "diff".to_string(),
            body: self.runtime_agent_diff_display(pane_id)?,
        })
    }

    /// Builds the live `/diff` display from the pane's current Git repository.
    pub(super) fn runtime_agent_diff_display(&self, pane_id: &str) -> Result<String> {
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let Some(repository_root) = runtime_git_repository_root(&working_directory)? else {
            return Ok(format!(
                "vcs=git status=unavailable cwd={} reason=not-a-git-repository source=runtime-vcs-diff",
                json_escape(&working_directory.to_string_lossy())
            ));
        };
        let staged_diff = runtime_git_text(
            &repository_root,
            &["diff", "--cached", "--no-ext-diff", "--no-color", "--"],
        )?;
        let worktree_diff = runtime_git_text(
            &repository_root,
            &["diff", "--no-ext-diff", "--no-color", "--"],
        )?;
        let untracked_files = runtime_git_untracked_files(&repository_root)?;
        let mut untracked_diffs = Vec::new();
        for file in &untracked_files {
            untracked_diffs.push(runtime_git_untracked_diff(&repository_root, file)?);
        }
        let mut lines = vec![format!(
            "vcs=git repository={} staged_diff_bytes={} worktree_diff_bytes={} untracked_files={} source=runtime-vcs-diff",
            json_escape(&repository_root.to_string_lossy()),
            staged_diff.len(),
            worktree_diff.len(),
            untracked_files.len()
        )];
        lines.push("[staged]".to_string());
        lines.push(if staged_diff.is_empty() {
            "(no staged changes)".to_string()
        } else {
            staged_diff
        });
        lines.push("[worktree]".to_string());
        lines.push(if worktree_diff.is_empty() {
            "(no unstaged changes)".to_string()
        } else {
            worktree_diff
        });
        lines.push("[untracked]".to_string());
        if untracked_files.is_empty() {
            lines.push("(no untracked files)".to_string());
        } else {
            for (file, diff) in untracked_files.iter().zip(untracked_diffs) {
                lines.push(format!("file={}", json_escape(file)));
                lines.push(diff);
            }
        }
        Ok(lines.join("\n"))
    }

    /// Executes `/init` by creating a project instruction scaffold.
    pub(super) fn execute_agent_shell_init_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("init command must be a slash command"))?;
        if !invocation.args.trim().is_empty() {
            return Err(MezError::invalid_args(
                "init slash command does not accept arguments",
            ));
        }
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
        let working_directory = self
            .pane_current_working_directory(pane_id)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        let target = working_directory.join("AGENTS.md");
        if target.exists() {
            return Ok(AgentShellCommandOutcome::Display {
                command: "init".to_string(),
                body: format!(
                    "path={} created=false existing=true source=runtime-init",
                    json_escape(&target.to_string_lossy())
                ),
            });
        }
        let scaffold = runtime_agent_init_scaffold().as_bytes().to_vec();
        if self.defer_project_instruction_writes {
            self.deferred_project_instruction_writes
                .push(DeferredProjectInstructionWrite {
                    path: target.clone(),
                    bytes: scaffold,
                });
        } else {
            std::fs::write(&target, &scaffold)?;
        }
        Ok(AgentShellCommandOutcome::Mutated {
            command: "init".to_string(),
            body: format!(
                "path={} created=true bytes={} source=runtime-init",
                json_escape(&target.to_string_lossy()),
                runtime_agent_init_scaffold().len()
            ),
            visibility,
        })
    }

    /// Executes `/copy` by copying the latest model-authored `say` text.
    pub(super) fn execute_agent_shell_copy_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("copy command must be a slash command"))?;
        let (body, mutated) =
            runtime_write_agent_copy_output_for_pane(self, pane_id, invocation.args.trim())?;
        if mutated {
            Ok(AgentShellCommandOutcome::Mutated {
                command: "copy".to_string(),
                body,
                visibility: self.agent_shell_visibility_for_pane(pane_id)?,
            })
        } else {
            Ok(AgentShellCommandOutcome::Display {
                command: "copy".to_string(),
                body,
            })
        }
    }

    /// Returns the latest model-authored `say` text retained for a pane.
    pub(super) fn latest_agent_copy_output_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<(String, String, String)> {
        self.agent_copy_outputs.get(pane_id).map(|output| {
            (
                output.turn_id.clone(),
                output.output.clone(),
                output.content_type.clone(),
            )
        })
    }

    /// Executes `/logout` through the runtime auth store.
    pub(super) fn execute_agent_shell_logout_command(
        &mut self,
        pane_id: &str,
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
        let Some(auth_store) = self.auth_store() else {
            return Ok(AgentShellCommandOutcome::Display {
                command: "logout".to_string(),
                body: "logged_out=false reason=auth-store-unavailable source=runtime-auth"
                    .to_string(),
            });
        };
        let changed = auth_store.logout()?;
        runtime_append_auth_logout_audit(self, changed)?;
        let body = format!("logged_out={changed} source=runtime-auth");
        Ok(AgentShellCommandOutcome::Mutated {
            command: "logout".to_string(),
            body,
            visibility,
        })
    }

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

    /// Executes `/statusline` against live pane frame status-line settings.
    pub(super) fn execute_agent_shell_statusline_command(
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
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("statusline command must be a slash command"))?;
        if invocation.args.trim().is_empty() {
            return Ok(AgentShellCommandOutcome::Display {
                command: "statusline".to_string(),
                body: self.runtime_agent_statusline_display(),
            });
        }
        let fields = runtime_statusline_fields(&invocation.args)?;
        self.pane_frames_enabled = true;
        self.pane_frame_visible_fields = fields.clone();
        self.pane_frame_template = runtime_statusline_template(&fields);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "statusline".to_string(),
            body: format!("{} changed=true", self.runtime_agent_statusline_display()),
            visibility,
        })
    }

    /// Builds the live `/statusline` display from pane frame status settings.
    pub(super) fn runtime_agent_statusline_display(&self) -> String {
        format!(
            "enabled={} fields={} template={} source=runtime-statusline",
            self.pane_frames_enabled,
            runtime_string_array_json(&self.pane_frame_visible_fields),
            json_escape(&self.pane_frame_template)
        )
    }

    /// Executes `/title` against the active runtime window title.
    pub(super) fn execute_agent_shell_title_command(
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
            .ok_or_else(|| MezError::invalid_args("title command must be a slash command"))?;
        if slash.args.trim().is_empty() {
            return Ok(AgentShellCommandOutcome::Display {
                command: "title".to_string(),
                body: self.runtime_agent_title_display(pane_id)?,
            });
        }
        let invocation = runtime_single_rename_window_invocation(&slash.args)?;
        execute_command(&mut self.session, primary_client_id, &invocation)?;
        let body = format!(
            "{} changed=true",
            self.runtime_agent_title_display(pane_id)?
        );
        Ok(AgentShellCommandOutcome::Mutated {
            command: "title".to_string(),
            body,
            visibility,
        })
    }

    /// Builds the live `/title` display for the active window and pane.
    pub(super) fn runtime_agent_title_display(&self, pane_id: &str) -> Result<String> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane_title = self
            .find_pane_title(pane_id)
            .unwrap_or_else(|| "unknown".to_string());
        Ok(format!(
            "window_id={} window_title={} pane={} pane_title={} source=runtime-title",
            json_escape(window.id.as_str()),
            json_escape(&window.title()),
            json_escape(pane_id),
            json_escape(&pane_title)
        ))
    }

    /// Executes `/status` against the live runtime status source.
    pub(super) fn execute_agent_shell_status_command(
        &self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        Ok(AgentShellCommandOutcome::Display {
            command: "status".to_string(),
            body: self.runtime_agent_status_display(pane_id)?,
        })
    }

    /// Builds the live `/status` display from runtime session state.
    pub(super) fn runtime_agent_status_display(&self, pane_id: &str) -> Result<String> {
        let session = self.agent_shell_store.get(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "agent shell session not found for pane",
            )
        })?;
        let agent_id = format!("agent-{pane_id}");
        let descriptor = self.find_pane_descriptor(pane_id);
        let window_id = descriptor
            .as_ref()
            .map(|descriptor| descriptor.window_id.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let current_working_directory = self
            .pane_current_working_directory(pane_id)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let active_scopes = self.subagent_scopes.active_write_scopes_for(&agent_id);
        let writable_roots = active_scopes
            .iter()
            .map(|scope| scope.scope.clone())
            .collect::<Vec<_>>();
        let latest_turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .rev()
            .find(|turn| turn.pane_id == pane_id);
        let latest_turn_id = latest_turn
            .map(|turn| turn.turn_id.as_str())
            .unwrap_or("none");
        let latest_turn_state = latest_turn
            .map(|turn| runtime_agent_turn_state_name(turn.state))
            .unwrap_or("none");
        let context_blocks = latest_turn
            .and_then(|turn| self.agent_turn_contexts.get(&turn.turn_id))
            .map(|context| context.blocks.len())
            .unwrap_or(0);
        let request_messages = latest_turn
            .and_then(|turn| self.agent_turn_executions.get(&turn.turn_id))
            .map(|execution| execution.request.messages.len())
            .unwrap_or(0);
        let token_usage_by_model = self
            .agent_token_usage_by_conversation
            .get(&session.session_id)
            .cloned()
            .unwrap_or_default();
        let running_turn = session
            .running_turn_id
            .as_deref()
            .unwrap_or("none")
            .to_string();
        let reasoning_profile = model_profile
            .reasoning_profile
            .as_deref()
            .unwrap_or("none")
            .to_string();
        let thinking = self
            .model_profile_thinking_enabled(&model_profile)
            .map(|enabled| if enabled { "enabled" } else { "disabled" })
            .unwrap_or("unsupported");
        let rows = vec![
            vec!["Pane".to_string(), session.pane_id.clone()],
            vec!["Session".to_string(), session.session_id.clone()],
            vec![
                "Visibility".to_string(),
                agent_shell_visibility_json_name(session.visibility).to_string(),
            ],
            vec!["Running turn".to_string(), running_turn],
            vec![
                "Transcript entries".to_string(),
                session.transcript_entries.to_string(),
            ],
            vec![
                "Directive".to_string(),
                session
                    .directive
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            ],
            vec![
                "Log level".to_string(),
                session.log_level.as_str().to_string(),
            ],
            vec!["Agent id".to_string(), agent_id],
            vec!["Window id".to_string(), window_id],
            vec!["Current directory".to_string(), current_working_directory],
            vec![
                "Model".to_string(),
                format!(
                    "{} via {} (profile: {}, reasoning: {})",
                    model_profile.model,
                    model_profile.provider,
                    model_profile_name,
                    reasoning_profile
                ),
            ],
            vec!["Thinking".to_string(), thinking.to_string()],
            vec![
                "Prompt profile".to_string(),
                format!("{AGENT_PROMPT_PROFILE_NAME} v{AGENT_PROMPT_PROFILE_VERSION}"),
            ],
            vec![
                "Permissions".to_string(),
                format!(
                    "preset {}, approval {}, bypass {}",
                    runtime_permission_preset_name(self.permission_policy.preset),
                    runtime_approval_policy_name(self.permission_policy.approval_policy),
                    self.permission_policy.approval_bypass()
                ),
            ],
            vec![
                "Command rules".to_string(),
                self.permission_policy.rules().len().to_string(),
            ],
            vec![
                "Writable roots".to_string(),
                format!(
                    "{} ({})",
                    if writable_roots.is_empty() {
                        "none".to_string()
                    } else {
                        writable_roots.join(", ")
                    },
                    writable_roots.len()
                ),
            ],
            vec![
                "Active write scopes".to_string(),
                self.subagent_scopes.active_write_scope_count().to_string(),
            ],
            vec![
                "Context".to_string(),
                format!(
                    "{context_blocks} blocks, {request_messages} request messages, window={} tokens, compaction=provider-rejection/manual",
                    model_profile.context_window_tokens()
                ),
            ],
            vec![
                "Provider tokens".to_string(),
                Self::runtime_agent_provider_token_usage_summary(&token_usage_by_model),
            ],
            vec![
                "Latest turn".to_string(),
                format!("{latest_turn_id} ({latest_turn_state})"),
            ],
        ];
        let mut lines = vec!["## Agent Status".to_string(), String::new()];
        lines.extend(runtime_markdown_table(&["Field", "Value"], &rows));
        if !token_usage_by_model.is_empty() {
            lines.push(String::new());
            lines.push("### Provider Token Usage".to_string());
            lines.extend(runtime_markdown_table(
                &[
                    "Provider",
                    "Model",
                    "Billed input",
                    "Cached input",
                    "Output",
                    "Reasoning",
                    "Cache Hit %",
                ],
                &Self::runtime_agent_provider_token_usage_rows(&token_usage_by_model),
            ));
        }
        if !active_scopes.is_empty() {
            let scope_rows = active_scopes
                .into_iter()
                .map(|scope| {
                    vec![
                        scope.scope,
                        scope.agent_id,
                        runtime_cooperation_mode_name(scope.mode).to_string(),
                        scope.serial_lock.unwrap_or_else(|| "none".to_string()),
                    ]
                })
                .collect::<Vec<_>>();
            lines.push(String::new());
            lines.push("### Writable Roots".to_string());
            lines.extend(runtime_markdown_table(
                &["Root", "Owner", "Mode", "Serial lock"],
                &scope_rows,
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Returns the compact `/status` summary for per-model provider tokens.
    fn runtime_agent_provider_token_usage_summary(
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> String {
        match usage_by_model.len() {
            0 => "none".to_string(),
            1 => usage_by_model
                .iter()
                .next()
                .map(|(key, usage)| {
                    format!(
                        "{}: {}",
                        key.display_name(),
                        Self::runtime_agent_provider_token_usage_metrics(*usage)
                    )
                })
                .unwrap_or_else(|| "none".to_string()),
            count => format!("{count} models; see Provider Token Usage"),
        }
    }

    /// Builds markdown table rows for per-model provider token accounting.
    fn runtime_agent_provider_token_usage_rows(
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) -> Vec<Vec<String>> {
        usage_by_model
            .iter()
            .map(|(key, usage)| {
                vec![
                    key.provider.clone(),
                    key.model.clone(),
                    usage.billed_input_tokens().to_string(),
                    usage.cached_input_tokens_display(),
                    usage.output_tokens.to_string(),
                    usage.reasoning_tokens.to_string(),
                    usage.cached_input_hit_ratio_display(),
                ]
            })
            .collect()
    }

    /// Formats one provider/model token usage value for compact displays.
    fn runtime_agent_provider_token_usage_metrics(usage: ModelTokenUsage) -> String {
        format!(
            "input={} (+ {} cached) cache_hit={} output={} reasoning={} total={}",
            usage.billed_input_tokens(),
            usage.cached_input_tokens_display(),
            usage.cached_input_hit_ratio_display(),
            usage.output_tokens,
            usage.reasoning_tokens,
            usage.total_tokens()
        )
    }

    /// Moves the current terminal view into history and clears the viewport.
    pub(super) fn clear_agent_shell_terminal_view(&mut self, pane_id: &str) -> Result<bool> {
        self.active_copy_modes.remove(pane_id);
        let Some(screen) = self.pane_screens.get_mut(pane_id) else {
            return Ok(false);
        };
        screen.clear_visible_into_history();
        Ok(true)
    }

    /// Runs the execute agent shell debug config command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_debug_config_command(
        &self,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("debug-config command must be a slash command")
        })?;
        let filter = invocation.args.split_whitespace().next();
        Ok(AgentShellCommandOutcome::Display {
            command: "debug-config".to_string(),
            body: self.runtime_debug_config_display(filter)?,
        })
    }

    /// Builds the live `/debug-config` display from effective runtime config state.
    pub(super) fn runtime_debug_config_display(&self, filter: Option<&str>) -> Result<String> {
        let effective = compose_effective_config(&self.config_layers)?;
        let mut lines = vec![format!(
            "layers={} applied_layers={} skipped_layers={} values={} diagnostics={} permission_preset={} approval_policy={} bypass={} providers={} model_profiles={} mcp_servers={} hooks={} source=runtime-config",
            self.config_layers.len(),
            effective.applied_layers().len(),
            effective.skipped_layers().len(),
            effective.values().len(),
            effective.diagnostics().len(),
            runtime_permission_preset_name(self.permission_policy.preset),
            runtime_approval_policy_name(self.permission_policy.approval_policy),
            self.permission_policy.approval_bypass(),
            self.provider_registry.providers.len(),
            self.provider_registry.profiles.len(),
            self.mcp_registry.list_servers().len(),
            self.hook_definitions.len()
        )];
        for (index, layer) in self.config_layers.iter().enumerate() {
            lines.push(format!(
                "layer={} index={} scope={} trusted={} applied={} skipped={} format={} path={}",
                json_escape(&layer.name),
                index,
                Self::runtime_config_scope_name(layer.scope),
                layer.trusted,
                effective.applied_layers().contains(&layer.name),
                effective.skipped_layers().contains(&layer.name),
                Self::runtime_config_format_name(layer.format),
                layer
                    .path
                    .as_ref()
                    .map(|path| json_escape(&path.to_string_lossy()))
                    .unwrap_or_else(|| "inline".to_string())
            ));
        }
        for diagnostic in effective.diagnostics() {
            lines.push(format!(
                "diagnostic path={} message={}",
                json_escape(&diagnostic.path),
                json_escape(&diagnostic.message)
            ));
        }
        for (path, value) in effective.values() {
            if filter.is_some_and(|filter| filter != path) {
                continue;
            }
            lines.push(format!(
                "value path={} source={} value={}",
                json_escape(path),
                json_escape(&value.source_layer),
                json_escape(&value.value)
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Runs the runtime config scope name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_config_scope_name(scope: ConfigScope) -> &'static str {
        match scope {
            ConfigScope::Primary => "primary",
            ConfigScope::ProjectOverlay => "project-overlay",
            ConfigScope::LiveOverride => "live-override",
        }
    }

    /// Runs the runtime config format name operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_config_format_name(format: ConfigFormat) -> &'static str {
        match format {
            ConfigFormat::Toml => "toml",
            ConfigFormat::Yaml => "yaml",
            ConfigFormat::Json => "json",
        }
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

/// Runs the runtime git repository root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_repository_root(working_directory: &PathBuf) -> Result<Option<PathBuf>> {
    let output = match runtime_git_output(working_directory, &["rev-parse", "--show-toplevel"]) {
        Ok(output) => output,
        Err(error) if error.kind() == crate::error::MezErrorKind::Io => return Ok(None),
        Err(error) => return Err(error),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(root)))
}

/// Runs the runtime git text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_text(repository_root: &PathBuf, args: &[&str]) -> Result<String> {
    let output = runtime_git_output(repository_root, args)?;
    if !output.status.success() {
        return Err(runtime_git_status_error(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Runs the runtime git untracked files operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_untracked_files(repository_root: &PathBuf) -> Result<Vec<String>> {
    let output = runtime_git_output(
        repository_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    if !output.status.success() {
        return Err(runtime_git_status_error(
            &["ls-files", "--others", "--exclude-standard", "-z"],
            &output,
        ));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| String::from_utf8_lossy(bytes).to_string())
        .collect())
}

/// Runs the runtime git untracked diff operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_untracked_diff(repository_root: &PathBuf, file: &str) -> Result<String> {
    let file_path = repository_root.join(file);
    let output = Command::new("git")
        .args(["diff", "--no-index", "--no-ext-diff", "--no-color", "--"])
        .arg("/dev/null")
        .arg(&file_path)
        .current_dir(repository_root)
        .output()
        .map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!("failed to run git diff for untracked file `{file}`: {error}"),
            )
        })?;
    match output.status.code() {
        Some(0 | 1) => Ok(String::from_utf8_lossy(&output.stdout).to_string()),
        _ => Err(runtime_git_status_error(
            &["diff", "--no-index", "--no-ext-diff", "--no-color"],
            &output,
        )),
    }
}

/// Runs the runtime statusline fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_statusline_fields(args: &str) -> Result<Vec<String>> {
    let fields = args
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .filter(|field| !field.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if fields.is_empty() {
        return Err(MezError::invalid_args(
            "statusline slash command requires at least one field",
        ));
    }
    for field in &fields {
        if !RUNTIME_STATUSLINE_FIELDS
            .iter()
            .any(|allowed| allowed == field)
        {
            return Err(MezError::invalid_args(format!(
                "unsupported statusline field `{field}`"
            )));
        }
    }
    Ok(fields)
}

/// Runs the runtime statusline template operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_statusline_template(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| format!("#{{{field}}}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Runs the runtime single mode arg operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_single_mode_arg(args: &str, command: &str, default: &str) -> Result<String> {
    let values = args.split_whitespace().collect::<Vec<_>>();
    if values.len() > 1 {
        return Err(MezError::invalid_args(format!(
            "{command} slash command accepts at most one argument"
        )));
    }
    Ok(values.first().copied().unwrap_or(default).to_string())
}

/// Runs the validate agent personality operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_agent_personality(value: &str) -> Result<()> {
    if value.len() > 64 {
        return Err(MezError::invalid_args(
            "personality slash command style must be 64 bytes or fewer",
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(MezError::invalid_args(
            "personality slash command style must not contain control characters",
        ));
    }
    Ok(())
}

/// Defines the RUNTIME STATUSLINE FIELDS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_STATUSLINE_FIELDS: &[&str] = &[
    "session.id",
    "window.id",
    "window.index",
    "window.list",
    "window.title",
    "window.name",
    "window.active",
    "window.pane_count",
    "pane.id",
    "pane.index",
    "pane.title",
    "pane.active",
    "pane.size",
    "pane.primary_pid",
    "pane.process_name",
    "pane.exit_status",
    "pane.mode",
    "agent.id",
    "agent.name",
    "agent.status",
    "agent.model",
    "agent.reasoning",
    "agent.thinking",
    "agent.context_usage",
    "policy.mode",
    "observer.pending_count",
    "history.position",
];

/// Runs the runtime git output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_output(repository_root: &PathBuf, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(repository_root)
        .output()
        .map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!("failed to run git {}: {error}", args.join(" ")),
            )
        })
}

/// Runs the runtime git status error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_git_status_error(args: &[&str], output: &std::process::Output) -> MezError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        "no stderr".to_string()
    } else {
        stderr
    };
    MezError::invalid_state(format!(
        "git {} exited with status {:?}: {}",
        args.join(" "),
        output.status.code(),
        detail
    ))
}

/// Runs the runtime single permissions invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_single_permissions_invocation(args: &str) -> Result<CommandInvocation> {
    let trimmed = args.trim();
    let command = if trimmed.is_empty() {
        "permissions".to_string()
    } else {
        let (head, tail) = trimmed
            .split_once(char::is_whitespace)
            .map(|(head, tail)| (head, tail.trim()))
            .unwrap_or((trimmed, ""));
        match head {
            "list" | "rules" => "list-command-rules".to_string(),
            "allow" => format!("allow-command {tail}"),
            "deny" => format!("deny-command {tail}"),
            "prompt" => format!("prompt-command {tail}"),
            "remove" | "delete" => format!("remove-command-rule {tail}"),
            "bypass" => {
                if tail.is_empty() {
                    "bypass-approvals status".to_string()
                } else {
                    format!("bypass-approvals {tail}")
                }
            }
            _ => format!("permissions {trimmed}"),
        }
    };
    let invocations = parse_command_sequence(&command)?;
    let [invocation] = invocations.as_slice() else {
        return Err(MezError::invalid_args(
            "permissions slash command accepts only one policy command",
        ));
    };
    if !matches!(
        invocation.name.as_str(),
        "permissions"
            | "list-command-rules"
            | "allow-command"
            | "deny-command"
            | "prompt-command"
            | "remove-command-rule"
            | "bypass-approvals"
    ) {
        return Err(MezError::invalid_args(
            "permissions slash command can only execute policy commands",
        ));
    }
    Ok(invocation.clone())
}

/// Runs the runtime single approval invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_single_approval_invocation(args: &str) -> Result<CommandInvocation> {
    let command = if args.trim().is_empty() {
        "approval".to_string()
    } else {
        format!("approval {args}")
    };
    let invocations = parse_command_sequence(&command)?;
    let [invocation] = invocations.as_slice() else {
        return Err(MezError::invalid_args(
            "approval slash command accepts only one approval command",
        ));
    };
    if invocation.name != "approval" {
        return Err(MezError::invalid_args(
            "approval slash command can only execute approval",
        ));
    }
    Ok(invocation.clone())
}

/// Runs the runtime single rename window invocation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_single_rename_window_invocation(args: &str) -> Result<CommandInvocation> {
    let invocations = parse_command_sequence(&format!("rename-window {args}"))?;
    let [invocation] = invocations.as_slice() else {
        return Err(MezError::invalid_args(
            "title slash command accepts only one title value",
        ));
    };
    if invocation.name != "rename-window" || invocation.target_arg().is_some() {
        return Err(MezError::invalid_args(
            "title slash command can only rename the active window",
        ));
    }
    Ok(invocation.clone())
}

/// Runs the runtime agent init scaffold operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_agent_init_scaffold() -> &'static str {
    "# Repository Guidelines\n\n\
## Project Structure\n\
- Document the main source, test, documentation, and generated-output directories.\n\n\
## Build, Test, and Development Commands\n\
- List the commands contributors should run before handing off changes.\n\n\
## Coding Style\n\
- Describe formatting, naming, review, and documentation expectations.\n\n\
## Security and Configuration\n\
- Note secret-handling rules, local overrides, generated files, and unsafe operations.\n"
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
}
