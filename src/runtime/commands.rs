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
    ProviderModelCatalog, ProviderModelInfo, ProviderQuotaUsage, ReqwestProviderHttpTransport,
    append_mcp_context, model_context_text_word_count, openai_default_reasoning_levels_for_model,
    openai_provider_from_auth_store_with_provider_options,
};
use crate::auth::AuthCredentialKind;
use crate::error::MezErrorKind;
use crate::readline::ReadlineEdit;
use crate::runtime::config::{
    runtime_default_models_for_provider, runtime_recommended_model_for_provider,
};
use base64::Engine;

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
                    && command == "auto-reasoning"
                {
                    let auto_reasoning_outcome =
                        self.execute_agent_shell_auto_reasoning_command(&pane_id, input)?;
                    runtime_agent_shell_command_response_json(
                        &pane_id,
                        input,
                        Some(&auto_reasoning_outcome),
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

    /// Runs the execute agent shell model command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_agent_shell_model_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("model command must be a slash command"))?;
        let agent_id = format!("agent-{pane_id}");
        let args = runtime_model_command_args(&invocation.args)?;
        if args.secondary {
            return self.execute_agent_shell_secondary_model_command(
                pane_id,
                &agent_id,
                RuntimeSecondaryModelCommandArgs {
                    profile: args.profile.as_deref(),
                    reasoning_profile: args.reasoning_profile.as_deref(),
                    clear: args.clear,
                    list: args.list,
                    show: args.show,
                },
            );
        }
        let scope = runtime_model_override_scope_for_args(self, pane_id, &agent_id, &args)?;
        let (active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        if args.clear {
            self.clear_model_profile_override(scope.clone());
            let (active_name, active_profile) =
                self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
            return Ok(AgentShellCommandOutcome::Mutated {
                command: "model".to_string(),
                body: format!(
                    "scope={} cleared=true active_profile={} provider={} model={}",
                    runtime_model_override_scope_name(&scope),
                    active_name,
                    active_profile.provider,
                    active_profile.model
                ),
                visibility: self
                    .agent_shell_store
                    .get(pane_id)
                    .map(|session| session.visibility)
                    .unwrap_or(AgentShellVisibility::Hidden),
            });
        }
        if args.list {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.record_agent_provider_quota_usage(pane_id, &catalog.quota_usage);
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_catalog_display(&active_name, &active_profile, &catalog),
            });
        }
        if args.profile.is_none() || args.show {
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_profile_display(
                    &active_name,
                    &active_profile,
                    self.provider_registry.profiles(),
                ),
            });
        }
        let requested = args
            .profile
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry.profile(requested).is_some()
        {
            requested.to_string()
        } else {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.runtime_generated_profile_for_provider_model(
                &active_profile.provider,
                requested,
                args.reasoning_profile.as_deref(),
                None,
                &catalog,
            )?
        };
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "model".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} source=runtime-model-selection",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none")
            ),
            visibility: self
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Runs the execute agent shell model command async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn execute_agent_shell_model_command_async(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("model command must be a slash command"))?;
        let agent_id = format!("agent-{pane_id}");
        let args = runtime_model_command_args(&invocation.args)?;
        if args.secondary {
            return self
                .execute_agent_shell_secondary_model_command_async(
                    pane_id,
                    &agent_id,
                    RuntimeSecondaryModelCommandArgs {
                        profile: args.profile.as_deref(),
                        reasoning_profile: args.reasoning_profile.as_deref(),
                        clear: args.clear,
                        list: args.list,
                        show: args.show,
                    },
                )
                .await;
        }
        let scope = runtime_model_override_scope_for_args(self, pane_id, &agent_id, &args)?;
        let (active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        if args.clear {
            self.clear_model_profile_override(scope.clone());
            let (active_name, active_profile) =
                self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
            return Ok(AgentShellCommandOutcome::Mutated {
                command: "model".to_string(),
                body: format!(
                    "scope={} cleared=true active_profile={} provider={} model={}",
                    runtime_model_override_scope_name(&scope),
                    active_name,
                    active_profile.provider,
                    active_profile.model
                ),
                visibility: self
                    .agent_shell_store
                    .get(pane_id)
                    .map(|session| session.visibility)
                    .unwrap_or(AgentShellVisibility::Hidden),
            });
        }
        if args.list {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.record_agent_provider_quota_usage(pane_id, &catalog.quota_usage);
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_catalog_display(&active_name, &active_profile, &catalog),
            });
        }
        if args.profile.is_none() || args.show {
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_profile_display(
                    &active_name,
                    &active_profile,
                    self.provider_registry.profiles(),
                ),
            });
        }
        let requested = args
            .profile
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry.profile(requested).is_some()
        {
            requested.to_string()
        } else {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.runtime_generated_profile_for_provider_model(
                &active_profile.provider,
                requested,
                args.reasoning_profile.as_deref(),
                None,
                &catalog,
            )?
        };
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "model".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} source=runtime-model-selection",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none")
            ),
            visibility: self
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Executes `/latency` as a pane-local model-profile latency preference override.
    pub(super) fn execute_agent_shell_latency_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("latency command must be a slash command"))?;
        let agent_id = format!("agent-{pane_id}");
        let (active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let args = invocation.args.trim();
        if args.is_empty() || matches!(args, "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "latency".to_string(),
                body: format!(
                    "active_profile={} provider={} model={} reasoning_profile={} latency_preference={} available={}",
                    active_name,
                    active_profile.provider,
                    active_profile.model,
                    active_profile
                        .reasoning_profile
                        .as_deref()
                        .unwrap_or("none"),
                    active_profile
                        .latency_preference
                        .as_deref()
                        .unwrap_or("default"),
                    RUNTIME_LATENCY_PREFERENCES.join(",")
                ),
            });
        }
        if args.split_whitespace().count() != 1 {
            return Err(MezError::invalid_args(
                "latency command accepts at most one preference",
            ));
        }
        let latency = runtime_validate_latency_preference(args)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &active_profile.provider,
            &active_profile.model,
            active_profile.reasoning_profile.as_deref(),
            Some(latency),
            &catalog,
        )?;
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "latency".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} latency_preference={} source=runtime-latency-selection",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none"),
                profile.latency_preference.as_deref().unwrap_or("default")
            ),
            visibility: self
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Executes `/model --secondary` against the auto-sizing router profile.
    ///
    /// The secondary model is the provider request used to classify turn size
    /// before the main model request. Keeping it on the `/model` command makes
    /// the model controls discoverable without changing ordinary pane model
    /// selection semantics.
    fn execute_agent_shell_secondary_model_command(
        &mut self,
        pane_id: &str,
        agent_id: &str,
        args: RuntimeSecondaryModelCommandArgs<'_>,
    ) -> Result<AgentShellCommandOutcome> {
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, agent_id, None)?;
        let (secondary_name, secondary_profile) = self.active_secondary_model_profile()?;
        if args.clear {
            return self.set_secondary_model_profile_outcome(
                pane_id,
                DEFAULT_AUTO_SIZING_ROUTER_PROFILE,
                &active_profile.provider,
                "runtime-secondary-model-clear",
            );
        }
        if args.list {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.record_agent_provider_quota_usage(pane_id, &catalog.quota_usage);
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_catalog_display(&secondary_name, &secondary_profile, &catalog),
            });
        }
        if args.profile.is_none() || args.show {
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_secondary_model_profile_display(
                    &secondary_name,
                    &secondary_profile,
                    &active_profile,
                ),
            });
        }
        let requested = args
            .profile
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry.profile(requested).is_some()
        {
            requested.to_string()
        } else {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.runtime_generated_profile_for_provider_model(
                &active_profile.provider,
                requested,
                args.reasoning_profile,
                None,
                &catalog,
            )?
        };
        self.set_secondary_model_profile_outcome(
            pane_id,
            &profile_name,
            &active_profile.provider,
            "runtime-secondary-model-selection",
        )
    }

    /// Async variant of `/model --secondary` for provider catalog lookups.
    async fn execute_agent_shell_secondary_model_command_async(
        &mut self,
        pane_id: &str,
        agent_id: &str,
        args: RuntimeSecondaryModelCommandArgs<'_>,
    ) -> Result<AgentShellCommandOutcome> {
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, agent_id, None)?;
        let (secondary_name, secondary_profile) = self.active_secondary_model_profile()?;
        if args.clear {
            return self.set_secondary_model_profile_outcome(
                pane_id,
                DEFAULT_AUTO_SIZING_ROUTER_PROFILE,
                &active_profile.provider,
                "runtime-secondary-model-clear",
            );
        }
        if args.list {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.record_agent_provider_quota_usage(pane_id, &catalog.quota_usage);
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_model_catalog_display(&secondary_name, &secondary_profile, &catalog),
            });
        }
        if args.profile.is_none() || args.show {
            return Ok(AgentShellCommandOutcome::Display {
                command: "model".to_string(),
                body: runtime_secondary_model_profile_display(
                    &secondary_name,
                    &secondary_profile,
                    &active_profile,
                ),
            });
        }
        let requested = args
            .profile
            .ok_or_else(|| MezError::invalid_args("model command requires a profile name"))?;
        let profile_name = if args.reasoning_profile.is_none()
            && self.provider_registry.profile(requested).is_some()
        {
            requested.to_string()
        } else {
            let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
            self.runtime_generated_profile_for_provider_model(
                &active_profile.provider,
                requested,
                args.reasoning_profile,
                None,
                &catalog,
            )?
        };
        self.set_secondary_model_profile_outcome(
            pane_id,
            &profile_name,
            &active_profile.provider,
            "runtime-secondary-model-selection",
        )
    }

    /// Returns the currently configured secondary auto-sizing model profile.
    fn active_secondary_model_profile(&self) -> Result<(String, ModelProfile)> {
        let profile_name = self.agent_auto_sizing.router_model_profile.clone();
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
        Ok((profile_name, profile))
    }

    /// Applies a secondary auto-sizing model profile after provider validation.
    fn set_secondary_model_profile_outcome(
        &mut self,
        pane_id: &str,
        profile_name: &str,
        active_provider: &str,
        source: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let profile = self.provider_registry.resolve_profile(profile_name)?;
        if profile.provider != active_provider {
            return Err(MezError::config(format!(
                "secondary model profile `{profile_name}` uses provider `{}`, but active provider is `{active_provider}`",
                profile.provider
            )));
        }
        self.agent_auto_sizing.router_model_profile = profile_name.to_string();
        Ok(AgentShellCommandOutcome::Mutated {
            command: "model".to_string(),
            body: format!(
                "scope=secondary profile={} provider={} model={} reasoning_profile={} source={}",
                json_escape(profile_name),
                json_escape(&profile.provider),
                json_escape(&profile.model),
                profile.reasoning_profile.as_deref().unwrap_or("none"),
                json_escape(source)
            ),
            visibility: self
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Returns configured provider model names for the pane's active provider.
    ///
    /// The picker path is intentionally synchronous, so it uses configured
    /// provider/profile metadata rather than live provider HTTP. The `/model
    /// list` command remains the richer async path for network-backed catalogs.
    pub(super) fn configured_model_names_for_pane(&mut self, pane_id: &str) -> Result<Vec<String>> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let active_model_label = format!("{}: {}", active_profile.provider, active_profile.model);
        let mut provider_ids: Vec<String> =
            self.provider_registry.providers().keys().cloned().collect();
        if let Some(auth_store) = self.auth_store.as_ref() {
            let all_metadata = auth_store.read_all_metadata().unwrap_or_default();
            for auth_provider in all_metadata.keys() {
                if !provider_ids.contains(auth_provider) {
                    provider_ids.push(auth_provider.clone());
                }
            }
        }
        let mut models = Vec::new();
        for provider_id in &provider_ids {
            let models_for_provider: Vec<String> = if let Some(provider_config) =
                self.provider_registry.provider(provider_id).cloned()
            {
                let catalog = self.runtime_model_catalog_for_provider(provider_id)?;
                let mut items: Vec<String> = catalog
                    .models
                    .iter()
                    .map(|model| format!("{provider_id}: {}", model.id))
                    .collect();
                let configured_items: Vec<String> = if provider_config.models.is_empty() {
                    runtime_default_models_for_provider(&provider_config.kind)
                        .map(|models| models.iter().map(|m| m.to_string()).collect())
                        .unwrap_or_default()
                } else {
                    provider_config.models.clone()
                }
                .iter()
                .map(|m| format!("{provider_id}: {m}"))
                .filter(|label| !items.iter().any(|i| i == label))
                .collect();
                items.extend(configured_items);
                items
            } else {
                runtime_default_models_for_provider(provider_id)
                    .map(|models| {
                        models
                            .iter()
                            .map(|m| format!("{provider_id}: {m}"))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            for label in models_for_provider {
                if !models.iter().any(|m: &String| m == &label) {
                    models.push(label);
                }
            }
        }
        if !models.iter().any(|m| m == &active_model_label) {
            models.insert(0, active_model_label);
        }
        Ok(models)
    }

    /// Returns configured reasoning choices for a pane model picker.
    pub(super) fn configured_reasoning_levels_for_pane_model(
        &mut self,
        pane_id: &str,
        model_name: &str,
    ) -> Result<Vec<String>> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let provider_config = self.provider_registry.provider(&active_profile.provider);
        let mut levels = catalog
            .models
            .iter()
            .find(|model| model.id == model_name)
            .map(|model| {
                if model.reasoning_levels.is_empty() {
                    catalog.reasoning_levels.clone()
                } else {
                    model.reasoning_levels.clone()
                }
            })
            .unwrap_or_else(|| {
                provider_config
                    .map(|provider_config| {
                        runtime_configured_reasoning_levels_for_model(provider_config, model_name)
                    })
                    .unwrap_or_default()
            });
        if let Some(reasoning) = active_profile.reasoning_profile
            && !levels.iter().any(|level| level == &reasoning)
        {
            levels.insert(0, reasoning);
        }
        Ok(dedupe_runtime_strings(levels))
    }

    /// Applies a model selected from the pane-frame model picker.
    pub(super) fn apply_pane_model_picker_selection(
        &mut self,
        pane_id: &str,
        model_label: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let agent_id = format!("agent-{pane_id}");
        let (provider_id, model_name) = parse_picker_model_label(model_label);
        let catalog = self.runtime_model_catalog_for_provider(provider_id)?;
        let requested_reasoning = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()
            .and_then(|(_name, active_profile)| {
                if active_profile.provider == provider_id {
                    active_profile.reasoning_profile
                } else {
                    None
                }
            })
            .filter(|reasoning| {
                catalog
                    .models
                    .iter()
                    .find(|model| model.id == model_name)
                    .map(|model| {
                        let levels = if model.reasoning_levels.is_empty() {
                            catalog.reasoning_levels.as_slice()
                        } else {
                            model.reasoning_levels.as_slice()
                        };
                        levels.is_empty() || levels.iter().any(|level| level == reasoning)
                    })
                    .unwrap_or(false)
            });
        let model_name = model_name.to_string();
        let requested_reasoning = requested_reasoning.as_deref();
        self.apply_pane_model_picker_profile(pane_id, &model_name, requested_reasoning, &catalog)
    }

    /// Applies a reasoning level selected from the pane-frame reasoning picker.
    pub(super) fn apply_pane_reasoning_picker_selection(
        &mut self,
        pane_id: &str,
        reasoning: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let model_name = active_profile.model.clone();
        self.apply_pane_model_picker_profile(pane_id, &model_name, Some(reasoning), &catalog)
    }

    /// Applies a model preset selected from the pane-frame preset picker.
    pub(super) fn apply_preset_selection(
        &mut self,
        pane_id: &str,
        preset_name: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let Some(preset) = self.preset_registry.resolve(preset_name).cloned() else {
            return Err(MezError::invalid_args(format!(
                "model preset `{preset_name}` is not configured"
            )));
        };
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, _active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let new_profile = self
            .provider_registry
            .resolve_profile(&preset.default_model_profile)?;
        let provider_id = new_profile.provider.clone();
        let model_name = new_profile.model.clone();
        let new_reasoning = new_profile.reasoning_profile.clone();
        let new_latency = new_profile.latency_preference.clone();
        let catalog = self.runtime_model_catalog_for_provider(&provider_id)?;
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &provider_id,
            &model_name,
            new_reasoning.as_deref(),
            new_latency.as_deref(),
            &catalog,
        )?;
        let router = preset.auto_sizing_router_model_profile.clone();
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let mut auto_sizing = self.runtime_auto_sizing_config_for_pane(pane_id).clone();
        auto_sizing.router_model_profile = router.clone();
        auto_sizing.small_model_profile = preset.auto_sizing_small_model_profile.clone();
        auto_sizing.medium_model_profile = preset.auto_sizing_medium_model_profile.clone();
        auto_sizing.large_model_profile = preset.auto_sizing_large_model_profile.clone();
        if !preset.allowed_reasoning_efforts.is_empty() {
            auto_sizing.allowed_reasoning_efforts = preset.allowed_reasoning_efforts.clone();
        }
        self.agent_auto_sizing_overrides
            .insert(pane_id.to_string(), auto_sizing);
        let resolved = self.provider_registry.resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "preset".to_string(),
            body: format!(
                "scope={} preset={} profile={} provider={} model={} reasoning_profile={} latency_preference={} router={} source=runtime-preset-selection",
                runtime_model_override_scope_name(&scope),
                preset_name,
                profile_name,
                resolved.provider,
                resolved.model,
                resolved.reasoning_profile.as_deref().unwrap_or("none"),
                resolved.latency_preference.as_deref().unwrap_or("default"),
                router,
            ),
            visibility: self
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Returns the auto-sizing configuration currently active for one pane.
    pub(super) fn runtime_auto_sizing_config_for_pane(
        &self,
        pane_id: &str,
    ) -> &RuntimeAutoSizingConfig {
        self.agent_auto_sizing_overrides
            .get(pane_id)
            .unwrap_or(&self.agent_auto_sizing)
    }

    /// Returns the preset label to render for one pane, when presets exist.
    pub(super) fn agent_preset_display_value_for_pane(&self, pane_id: &str) -> Option<String> {
        if !self.preset_registry.has_presets() {
            return None;
        }
        Some(
            self.active_model_preset_name_for_pane(pane_id)
                .unwrap_or_else(|| "custom".to_string()),
        )
    }

    /// Returns the active model preset name when the pane state matches one.
    pub(super) fn active_model_preset_name_for_pane(&self, pane_id: &str) -> Option<String> {
        if !self.preset_registry.has_presets() {
            return None;
        }
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()?;
        let auto_sizing = self.runtime_auto_sizing_config_for_pane(pane_id);
        self.preset_registry
            .presets
            .iter()
            .find_map(|(preset_name, preset)| {
                let preset_profile = self
                    .provider_registry
                    .resolve_profile(&preset.default_model_profile)
                    .ok()?;
                (runtime_model_profile_matches_preset_profile(&active_profile, &preset_profile)
                    && runtime_auto_sizing_matches_preset(auto_sizing, preset))
                .then(|| preset_name.clone())
            })
    }

    /// Applies a latency preference selected from the pane-frame latency picker.
    pub(super) fn apply_pane_latency_picker_selection(
        &mut self,
        pane_id: &str,
        latency: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let agent_id = format!("agent-{pane_id}");
        let (_active_name, active_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let latency = runtime_validate_latency_preference(latency)?;
        let catalog = self.runtime_model_catalog_for_provider(&active_profile.provider)?;
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &active_profile.provider,
            &active_profile.model,
            active_profile.reasoning_profile.as_deref(),
            Some(latency),
            &catalog,
        )?;
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "latency".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} latency_preference={} source=runtime-latency-picker",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none"),
                profile.latency_preference.as_deref().unwrap_or("default")
            ),
            visibility: self
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }

    /// Applies a generated pane-scoped model profile for picker selections.
    fn apply_pane_model_picker_profile(
        &mut self,
        pane_id: &str,
        model_name: &str,
        reasoning: Option<&str>,
        catalog: &RuntimeModelCatalog,
    ) -> Result<AgentShellCommandOutcome> {
        let provider_id = catalog.provider.clone();
        let profile_name = self.runtime_generated_profile_for_provider_model(
            &provider_id,
            model_name,
            reasoning,
            None,
            catalog,
        )?;
        let scope = RuntimeModelProfileOverrideScope::Pane(pane_id.to_string());
        self.set_model_profile_override(scope.clone(), &profile_name)?;
        let profile = self.provider_registry.resolve_profile(&profile_name)?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "model".to_string(),
            body: format!(
                "scope={} profile={} provider={} model={} reasoning_profile={} source=runtime-model-picker",
                runtime_model_override_scope_name(&scope),
                profile_name,
                profile.provider,
                profile.model,
                profile.reasoning_profile.as_deref().unwrap_or("none")
            ),
            visibility: self
                .agent_shell_store
                .get(pane_id)
                .map(|session| session.visibility)
                .unwrap_or(AgentShellVisibility::Hidden),
        })
    }
}

/// Parses a picker model label in `provider: model` format.
fn parse_picker_model_label(label: &str) -> (&str, &str) {
    match label.split_once(": ") {
        Some((provider, model)) => (provider, model),
        None => ("openai", label),
    }
}

/// Reports whether a live model profile is equivalent to a preset default.
fn runtime_model_profile_matches_preset_profile(
    active: &ModelProfile,
    preset: &ModelProfile,
) -> bool {
    active.provider == preset.provider
        && active.model == preset.model
        && active.reasoning_profile == preset.reasoning_profile
        && active.latency_preference.as_deref().unwrap_or("default")
            == preset.latency_preference.as_deref().unwrap_or("default")
}

/// Reports whether one auto-sizing configuration matches a preset.
fn runtime_auto_sizing_matches_preset(
    config: &RuntimeAutoSizingConfig,
    preset: &RuntimeModelPreset,
) -> bool {
    config.router_model_profile == preset.auto_sizing_router_model_profile
        && config.small_model_profile == preset.auto_sizing_small_model_profile
        && config.medium_model_profile == preset.auto_sizing_medium_model_profile
        && config.large_model_profile == preset.auto_sizing_large_model_profile
        && (preset.allowed_reasoning_efforts.is_empty()
            || config.allowed_reasoning_efforts == preset.allowed_reasoning_efforts)
}

impl RuntimeSessionService {
    fn runtime_model_catalog_for_provider(
        &mut self,
        provider_id: &str,
    ) -> Result<RuntimeModelCatalog> {
        if let Some(catalog) = self.provider_model_catalog_cache.get(provider_id) {
            return Ok(catalog.clone());
        }
        let provider_config = self
            .provider_registry
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!("provider `{provider_id}` is not configured"))
            })?;
        let fallback = runtime_configured_model_catalog(
            provider_id,
            &provider_config,
            &self.provider_registry,
        );
        if let Some(provider_error) =
            self.runtime_cached_model_catalog_miss_reason(&provider_config)
        {
            return Ok(RuntimeModelCatalog {
                provider: fallback.provider,
                source: fallback.source,
                provider_error: Some(provider_error),
                models: fallback.models,
                reasoning_levels: fallback.reasoning_levels,
                quota_usage: fallback.quota_usage,
            });
        }
        if provider_config.kind.as_str() == "openai" && fallback.models.is_empty() {
            return Err(MezError::invalid_state(
                "OpenAI model listing requires cached provider information or configured fallback models",
            ));
        }
        Ok(fallback)
    }

    /// Runs the runtime model catalog for provider async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn runtime_model_catalog_for_provider_async(
        &mut self,
        provider_id: &str,
    ) -> Result<RuntimeModelCatalog> {
        if let Some(catalog) = self.provider_model_catalog_cache.get(provider_id) {
            return Ok(catalog.clone());
        }
        let provider_config = self
            .provider_registry
            .provider(provider_id)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!("provider `{provider_id}` is not configured"))
            })?;
        let fallback = runtime_configured_model_catalog(
            provider_id,
            &provider_config,
            &self.provider_registry,
        );
        match provider_config.kind.as_str() {
            "openai" => match self
                .runtime_openai_model_catalog_async(&provider_config)
                .await
            {
                Ok(catalog) => {
                    let catalog = RuntimeModelCatalog::from_provider(catalog);
                    self.provider_model_catalog_cache
                        .insert(provider_id.to_string(), catalog.clone());
                    Ok(catalog)
                }
                Err(error) if fallback.models.is_empty() => Err(error),
                Err(error) => Ok(RuntimeModelCatalog {
                    provider: fallback.provider,
                    source: "config".to_string(),
                    provider_error: Some(error.message().to_string()),
                    models: fallback.models,
                    reasoning_levels: fallback.reasoning_levels,
                    quota_usage: Vec::new(),
                }),
            },
            _ => Ok(fallback),
        }
    }

    /// Explains why a cached provider catalog is not currently available
    /// without attempting network provider discovery.
    fn runtime_cached_model_catalog_miss_reason(
        &self,
        provider_config: &crate::runtime::RuntimeProviderConfig,
    ) -> Option<String> {
        if provider_config.kind.as_str() != "openai" {
            return None;
        }
        let Some(auth_store) = self.auth_store.as_ref() else {
            return Some("OpenAI model listing requires an attached auth store".to_string());
        };
        let metadata = match auth_store.read_metadata_for_provider("openai") {
            Ok(metadata) => metadata,
            Err(error) => return Some(error.message().to_string()),
        };
        let Some(metadata) = metadata else {
            return Some("OpenAI model listing requires an authenticated provider".to_string());
        };
        if metadata.credential_kind == AuthCredentialKind::ChatGpt {
            return Some(
                "ChatGPT browser credentials do not expose an OpenAI-compatible model catalog"
                    .to_string(),
            );
        }
        Some("OpenAI model catalog has not been refreshed; run :refresh-provider-info".to_string())
    }

    /// Refreshes cached provider information for every configured provider.
    pub(crate) async fn refresh_provider_info_async(&mut self) -> Result<String> {
        let provider_ids = self
            .provider_registry
            .providers()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut refreshed = 0usize;
        let mut failed = 0usize;
        let mut lines = Vec::new();
        for provider_id in &provider_ids {
            self.provider_model_catalog_cache.remove(provider_id);
            match self
                .runtime_model_catalog_for_provider_async(provider_id)
                .await
            {
                Ok(catalog) => {
                    refreshed = refreshed.saturating_add(1);
                    self.provider_model_catalog_cache
                        .insert(provider_id.clone(), catalog.clone());
                    let provider_error = catalog
                        .provider_error
                        .as_deref()
                        .map(runtime_model_catalog_unavailable_reason)
                        .unwrap_or_else(|| "none".to_string());
                    lines.push(format!(
                        "{} source={} models={} reasoning_levels={} quota_entries={} provider_error={}",
                        json_escape(provider_id),
                        json_escape(&catalog.source),
                        catalog.models.len(),
                        catalog.reasoning_levels.len(),
                        catalog.quota_usage.len(),
                        provider_error
                    ));
                }
                Err(error) => {
                    failed = failed.saturating_add(1);
                    lines.push(format!(
                        "{} refresh=failed error={}",
                        json_escape(provider_id),
                        json_escape(error.message())
                    ));
                }
            }
        }
        let mut body = format!(
            "providers={} refreshed={} failed={}",
            provider_ids.len(),
            refreshed,
            failed
        );
        if !lines.is_empty() {
            body.push('\n');
            body.push_str(&lines.join("\n"));
        }
        Ok(body)
    }

    /// Seeds the live model catalog cache for focused runtime tests.
    #[cfg(test)]
    pub(super) fn cache_provider_model_catalog_for_tests(
        &mut self,
        provider_id: &str,
        models: Vec<ProviderModelInfo>,
        reasoning_levels: Vec<String>,
    ) {
        self.provider_model_catalog_cache.insert(
            provider_id.to_string(),
            RuntimeModelCatalog {
                provider: provider_id.to_string(),
                source: "provider".to_string(),
                provider_error: None,
                models,
                reasoning_levels,
                quota_usage: Vec::new(),
            },
        );
    }

    /// Runs the runtime openai model catalog async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn runtime_openai_model_catalog_async(
        &mut self,
        provider_config: &crate::runtime::RuntimeProviderConfig,
    ) -> Result<ProviderModelCatalog> {
        self.append_credential_access_audit(
            "openai",
            &provider_config.auth_profile,
            "provider_model_list",
            "requested",
        )?;
        let Some(auth_store) = self.auth_store.as_ref() else {
            self.append_credential_access_audit(
                "openai",
                &provider_config.auth_profile,
                "provider_model_list",
                "denied",
            )?;
            return Err(MezError::invalid_state(
                "OpenAI model listing requires an attached auth store",
            ));
        };
        let metadata = auth_store
            .read_metadata_for_provider("openai")?
            .ok_or_else(|| {
                MezError::invalid_state("OpenAI model listing requires an authenticated provider")
            })?;
        if metadata.credential_kind == AuthCredentialKind::ChatGpt {
            self.append_credential_access_audit(
                "openai",
                &provider_config.auth_profile,
                "provider_model_list",
                "unsupported",
            )?;
            return Err(MezError::invalid_state(
                "ChatGPT browser credentials do not expose an OpenAI-compatible model catalog",
            ));
        }
        let endpoint_override = provider_config
            .base_url
            .as_deref()
            .filter(|endpoint| !endpoint.is_empty());
        let provider_result = openai_provider_from_auth_store_with_provider_options(
            auth_store,
            endpoint_override,
            &provider_config.options,
            DEFAULT_PROVIDER_TIMEOUT_MS,
            ReqwestProviderHttpTransport,
        );
        let provider = match provider_result {
            Ok(provider) => {
                self.append_credential_access_audit(
                    "openai",
                    &provider_config.auth_profile,
                    "provider_model_list",
                    "granted",
                )?;
                provider
            }
            Err(error) => {
                self.append_credential_access_audit(
                    "openai",
                    &provider_config.auth_profile,
                    "provider_model_list",
                    "denied",
                )?;
                return Err(error);
            }
        };
        provider.list_models_async().await
    }

    /// Runs the runtime generated profile for provider model operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn runtime_generated_profile_for_provider_model(
        &mut self,
        provider_id: &str,
        model_name: &str,
        reasoning_profile: Option<&str>,
        latency_preference: Option<&str>,
        catalog: &RuntimeModelCatalog,
    ) -> Result<String> {
        if model_name.trim().is_empty() {
            return Err(MezError::invalid_args("model name must not be empty"));
        }
        let model = catalog
            .models
            .iter()
            .find(|model| model.id == model_name)
            .ok_or_else(|| {
                MezError::invalid_args(format!(
                    "model `{model_name}` is not available from provider `{provider_id}`; run `/model list`"
                ))
            })?;
        if let Some(reasoning) = reasoning_profile {
            if reasoning.trim().is_empty() {
                return Err(MezError::invalid_args("reasoning level must not be empty"));
            }
            let levels = if model.reasoning_levels.is_empty() {
                catalog.reasoning_levels.as_slice()
            } else {
                model.reasoning_levels.as_slice()
            };
            if !levels.is_empty() && !levels.iter().any(|level| level == reasoning) {
                return Err(MezError::invalid_args(format!(
                    "reasoning level `{reasoning}` is not available for model `{model_name}`; available={}",
                    levels.join(",")
                )));
            }
        }

        let mut provider_options = std::collections::BTreeMap::new();
        if let Some(reasoning) = reasoning_profile {
            provider_options.insert("reasoning_effort".to_string(), reasoning.to_string());
        }
        let latency_preference = latency_preference
            .map(runtime_validate_latency_preference)
            .transpose()?
            .map(str::to_string);
        let profile = ModelProfile {
            provider: provider_id.to_string(),
            model: model_name.to_string(),
            reasoning_profile: reasoning_profile.map(str::to_string),
            latency_preference,
            multimodal_required: false,
            provider_options,
            safety_tier: None,
        };
        let profile_name = runtime_generated_model_profile_name(
            &self.provider_registry,
            provider_id,
            model_name,
            reasoning_profile,
            &profile,
        );
        self.provider_registry
            .profiles
            .entry(profile_name.clone())
            .or_insert(profile);
        Ok(profile_name)
    }

    /// Executes `/compact` by queuing model-backed conversation compaction.
    pub(super) fn execute_agent_shell_compact_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("compact command must be a slash command"))?;
        if !invocation.args.trim().is_empty() {
            return Err(MezError::invalid_args(
                "compact command does not accept arguments",
            ));
        }
        self.queue_agent_shell_compaction_with_model(pane_id, "manual")
    }

    /// Queues model-backed conversation compaction and marks the pane active.
    ///
    /// Manual `/compact` is submitted through synchronous prompt input, so it
    /// must publish visible state and return before provider I/O starts. The
    /// async provider service claims the queued task and reports completion
    /// through the runtime event loop.
    fn queue_agent_shell_compaction_with_model(
        &mut self,
        pane_id: &str,
        source: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let (conversation_id, transcript_entries, visibility, running_turn_id) = {
            let session = self.agent_shell_store.get(pane_id).ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
            (
                session.session_id.clone(),
                session.transcript_entries,
                session.visibility,
                session.running_turn_id.clone(),
            )
        };
        if let Some(turn_id) = running_turn_id {
            return Err(MezError::conflict(format!(
                "cannot compact conversation while turn {turn_id} is running"
            )));
        }
        if self.agent_compacting_panes.contains_key(pane_id) {
            return Err(MezError::conflict(format!(
                "cannot compact conversation while pane {pane_id} is already compacting"
            )));
        }
        if transcript_entries == 0 {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: compact skipped; no transcript entries are available",
            )?;
            return Ok(AgentShellCommandOutcome::Display {
                command: "compact".to_string(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries=0 summarized_entries=0 compacted=false reason=no-transcript-entries source=model-compact trigger={}",
                    json_escape(pane_id),
                    json_escape(&conversation_id),
                    json_escape(source)
                ),
            });
        }
        let transcript_records =
            self.inspect_agent_shell_transcript_for_compaction(&conversation_id)?;
        if transcript_records.is_empty() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: compact skipped; no durable transcript entries are available",
            )?;
            return Ok(AgentShellCommandOutcome::Display {
                command: "compact".to_string(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries={} summarized_entries=0 compacted=false reason=no-durable-transcript source=model-compact trigger={}",
                    json_escape(pane_id),
                    json_escape(&conversation_id),
                    transcript_entries,
                    json_escape(source)
                ),
            });
        }

        let agent_id = format!("agent-{pane_id}");
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let retained_tail_percent = self.agent_compaction_raw_retention_percent;
        let retained_transcript_entries = if source == "manual" {
            runtime_compact_forced_retained_transcript_entries(
                transcript_entries,
                &transcript_records,
                model_profile.context_window_budget_words(),
                retained_tail_percent,
            )
        } else {
            runtime_compact_retained_transcript_entries(
                transcript_entries,
                &transcript_records,
                model_profile.context_window_budget_words(),
                retained_tail_percent,
            )
        };
        let compactable_transcript_records = runtime_compact_transcript_entries_for_summary(
            transcript_entries,
            &transcript_records,
            retained_transcript_entries,
        );
        if compactable_transcript_records.is_empty() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: compact skipped; recent transcript tail already fits the active context budget",
            )?;
            return Ok(AgentShellCommandOutcome::Display {
                command: "compact".to_string(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries={} summarized_entries=0 remaining_transcript_entries={} compacted=false reason=within-retained-context-tail retained_context_tail_percent={} source=model-compact trigger={}",
                    json_escape(pane_id),
                    json_escape(&conversation_id),
                    transcript_entries,
                    retained_transcript_entries,
                    retained_tail_percent,
                    json_escape(source)
                ),
            });
        }

        let compaction_context =
            self.agent_context_for_pane_prompt(pane_id, "[context compaction requested]", 100)?;
        let compaction_context =
            self.apply_agent_shell_preference_context(pane_id, compaction_context)?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let compaction_context = runtime_compaction_context_without_transcript_blocks(
            append_mcp_context(compaction_context, &mcp_summary)?,
        )?;
        let summarized_entries = compactable_transcript_records.len();
        let request = runtime_model_compaction_request(
            &model_profile,
            pane_id,
            &conversation_id,
            transcript_entries,
            compactable_transcript_records,
            &compaction_context,
        )?;
        self.agent_compacting_panes
            .insert(pane_id.to_string(), current_unix_seconds().max(1));
        self.pending_agent_compaction_tasks.insert(
            pane_id.to_string(),
            RuntimeAgentCompactionTask {
                pane_id: pane_id.to_string(),
                conversation_id: conversation_id.clone(),
                source: source.to_string(),
                transcript_entries,
                retained_transcript_entries,
                summarized_entries,
                model_profile_name: model_profile_name.clone(),
                model_profile: model_profile.clone(),
                request,
            },
        );
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: compacting conversation summary trigger={} provider={} model={} previous_transcript_entries={} summarized_entries={}",
                source,
                model_profile.provider,
                model_profile.model,
                transcript_entries,
                summarized_entries
            ),
        )?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "compact".to_string(),
            body: format!(
                "pane={} conversation={} previous_transcript_entries={} summarized_entries={} compacted=false state=queued source=model-compact trigger={} model_profile={} provider={} model={}",
                json_escape(pane_id),
                json_escape(&conversation_id),
                transcript_entries,
                summarized_entries,
                json_escape(source),
                json_escape(&model_profile_name),
                json_escape(&model_profile.provider),
                json_escape(&model_profile.model)
            ),
            visibility,
        })
    }

    /// Executes `/compact` by asking the active model to produce the durable
    /// conversation summary that replaces older transcript context.
    pub(super) async fn execute_agent_shell_compact_command_async(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("compact command must be a slash command"))?;
        if !invocation.args.trim().is_empty() {
            return Err(MezError::invalid_args(
                "compact command does not accept arguments",
            ));
        }
        self.queue_agent_shell_compaction_with_model(pane_id, "manual")
    }

    /// Returns pane ids with queued model-backed compaction tasks.
    pub fn pending_agent_compaction_tasks(&self) -> Vec<String> {
        self.pending_agent_compaction_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Claims one queued compaction task for execution outside the actor.
    pub fn claim_agent_compaction_task(
        &mut self,
        pane_id: &str,
    ) -> Result<Option<RuntimeAgentCompactionDispatch>> {
        let Some(task) = self.pending_agent_compaction_tasks.remove(pane_id) else {
            return Ok(None);
        };
        if !self.agent_compacting_panes.contains_key(pane_id) {
            return Ok(None);
        }
        let provider_config = self
            .provider_registry
            .provider(&task.model_profile.provider)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!(
                    "provider `{}` for active model profile is not configured",
                    task.model_profile.provider
                ))
            })?;
        if provider_config.kind != "openai" {
            return Err(MezError::config(format!(
                "provider kind `{}` is not supported for model compaction",
                provider_config.kind
            )));
        }
        self.append_credential_access_audit(
            "openai",
            &provider_config.auth_profile,
            "provider_compact",
            "requested",
        )?;
        let Some(auth_store) = self.auth_store.as_ref() else {
            self.append_credential_access_audit(
                "openai",
                &provider_config.auth_profile,
                "provider_compact",
                "denied",
            )?;
            return Err(MezError::invalid_state(
                "OpenAI provider compaction requires an attached auth store",
            ));
        };
        let endpoint_override = provider_config
            .base_url
            .as_deref()
            .filter(|endpoint| !endpoint.is_empty());
        let provider = openai_provider_from_auth_store_with_provider_options(
            auth_store,
            endpoint_override,
            &provider_config.options,
            DEFAULT_PROVIDER_TIMEOUT_MS,
            ReqwestProviderHttpTransport,
        )?;
        self.append_credential_access_audit(
            "openai",
            &provider_config.auth_profile,
            "provider_compact",
            "granted",
        )?;
        self.claimed_agent_compaction_tasks
            .insert(pane_id.to_string(), task.clone());
        Ok(Some(RuntimeAgentCompactionDispatch {
            task,
            provider: RuntimeAgentProviderDispatchProvider::OpenAi(provider),
        }))
    }

    /// Applies a completed model-backed compaction response.
    pub fn apply_agent_compaction_completed_event(
        &mut self,
        pane_id: &str,
        response: ModelResponse,
    ) -> Result<bool> {
        let Some(task) = self.claimed_agent_compaction_tasks.remove(pane_id) else {
            self.agent_compacting_panes.remove(pane_id);
            return Ok(false);
        };
        self.agent_compacting_panes.remove(pane_id);
        self.record_agent_provider_token_usage_with_profile(
            pane_id,
            response.usage,
            response.usage,
            Some(&task.model_profile),
        );
        self.record_agent_provider_quota_usage(pane_id, &response.quota_usage);
        let summary = match runtime_model_compaction_summary_from_response(&response) {
            Ok(summary) => summary,
            Err(error) => {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!(
                        "agent: compact failed while reading summary: {}",
                        error.message()
                    ),
                )?;
                return Ok(true);
            }
        };
        let now = current_unix_seconds().max(1);
        let memory_id = format!("compact-{}", task.conversation_id);
        let content = runtime_model_compact_memory_content(
            pane_id,
            &task.conversation_id,
            task.transcript_entries,
            task.summarized_entries,
            &task.model_profile_name,
            &task.model_profile,
            &summary,
        );
        self.upsert_session_memory(MemoryRecord {
            id: memory_id.clone(),
            scope: MemoryScope::Pane {
                session_id: self.session.id.to_string(),
                pane_id: pane_id.to_string(),
            },
            created_at_unix_seconds: now,
            updated_at_unix_seconds: now,
            source: MemorySource::Agent,
            priority: 224,
            content,
            explicit_sensitive_consent: false,
        })?;
        let remaining_transcript_entries = self
            .agent_shell_store
            .retain_recent_transcript_entries(pane_id, task.retained_transcript_entries)?
            .transcript_entries;
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: compacted conversation summary memory_id={} summarized_entries={} remaining_transcript_entries={} source=model-compact trigger={}",
                memory_id, task.summarized_entries, remaining_transcript_entries, task.source
            ),
        )?;
        Ok(true)
    }

    /// Applies a failed model-backed compaction worker result.
    pub fn apply_agent_compaction_failed_event(
        &mut self,
        pane_id: &str,
        message: &str,
    ) -> Result<bool> {
        let had_task = self
            .pending_agent_compaction_tasks
            .remove(pane_id)
            .is_some()
            || self
                .claimed_agent_compaction_tasks
                .remove(pane_id)
                .is_some()
            || self.agent_compacting_panes.remove(pane_id).is_some();
        if had_task {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent: compact failed during provider request: {message}"),
            )?;
        }
        Ok(had_task)
    }

    /// Runs the inspect agent shell transcript for compaction operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn inspect_agent_shell_transcript_for_compaction(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let Some(store) = self.agent_transcript_store.as_ref() else {
            return Ok(Vec::new());
        };
        match store.inspect(conversation_id) {
            Ok(entries) => Ok(entries),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    /// Executes `/auto-reasoning` against pane-scoped auto-sizing state.
    pub(super) fn execute_agent_shell_auto_reasoning_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?.ok_or_else(|| {
            MezError::invalid_args("auto-reasoning command must be a slash command")
        })?;
        let mode = runtime_single_mode_arg(&invocation.args, "auto-reasoning", "toggle")?;
        let default_enabled = self.agent_auto_reasoning;
        let enabled_before = self
            .agent_auto_reasoning_overrides
            .get(pane_id)
            .copied()
            .unwrap_or(default_enabled);
        if matches!(mode.as_str(), "status" | "show") {
            return Ok(AgentShellCommandOutcome::Display {
                command: "auto-reasoning".to_string(),
                body: format!(
                    "pane={} enabled={} default={} override_present={} source=runtime-auto-reasoning",
                    json_escape(pane_id),
                    enabled_before,
                    default_enabled,
                    self.agent_auto_reasoning_overrides.contains_key(pane_id)
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
                    "auto-reasoning slash command expects on, off, toggle, status, or no argument",
                ));
            }
        };
        self.agent_auto_reasoning_overrides
            .insert(pane_id.to_string(), enabled);
        Ok(AgentShellCommandOutcome::Mutated {
            command: "auto-reasoning".to_string(),
            body: format!(
                "pane={} enabled={} default={} changed={} source=runtime-auto-reasoning",
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
        if let Some(auto_reasoning_enabled) = profile.auto_reasoning_enabled {
            self.agent_auto_reasoning_overrides
                .insert(pane_id.to_string(), auto_reasoning_enabled);
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
            let session = self.agent_shell_store.bind_conversation(
                &started.pane_id,
                &summary.conversation_id,
                summary.entries as u64,
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
                    summary.initial_prompt.as_deref().unwrap_or("-"),
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
                TranscriptRole::Assistant => "agent> ",
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
        let token_usage = self
            .agent_token_usage_by_conversation
            .get(&session.session_id)
            .copied()
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
                    "{context_blocks} blocks, {request_messages} request messages, window={} tokens, auto_compact={} threshold={:.2}",
                    model_profile.context_window_tokens(),
                    self.agent_auto_compact,
                    self.agent_auto_compact_threshold
                ),
            ],
            vec![
                "Provider tokens".to_string(),
                format!(
                    "input={} raw_input={} output={} reasoning={} cached_input={} cache_hit={} total={}",
                    token_usage.billed_input_tokens(),
                    token_usage.input_tokens,
                    token_usage.output_tokens,
                    token_usage.reasoning_tokens,
                    token_usage.cached_input_tokens_display(),
                    token_usage.cached_input_hit_ratio_display(),
                    token_usage.total_tokens()
                ),
            ],
            vec![
                "Latest turn".to_string(),
                format!("{latest_turn_id} ({latest_turn_state})"),
            ],
        ];
        let mut lines = vec!["## Agent Status".to_string(), String::new()];
        lines.extend(runtime_markdown_table(&["Field", "Value"], &rows));
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

    /// Runs the active model profile for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn active_model_profile_for_pane(
        &self,
        pane_id: &str,
        agent_id: &str,
        subagent_id: Option<&str>,
    ) -> Result<(String, ModelProfile)> {
        let default_profile = self
            .provider_registry
            .default_profile_name()
            .ok_or_else(|| MezError::config("default model profile is not configured"))?;
        let window_id = self
            .find_pane_descriptor(pane_id)
            .map(|descriptor| descriptor.window_id.to_string());
        let overrides = ModelProfileOverrides {
            default_profile: self
                .agent_selected_personality_profile(pane_id)
                .and_then(|profile| profile.model_profile.clone()),
            session_profile: self.model_profile_overrides.session_profile.clone(),
            window_profile: window_id.as_deref().and_then(|id| {
                self.model_profile_overrides
                    .window_profiles
                    .get(id)
                    .cloned()
            }),
            pane_profile: self
                .model_profile_overrides
                .pane_profiles
                .get(pane_id)
                .cloned(),
            agent_profile: self
                .model_profile_overrides
                .agent_profiles
                .get(agent_id)
                .cloned(),
            subagent_profile: subagent_id.and_then(|id| {
                self.model_profile_overrides
                    .subagent_profiles
                    .get(id)
                    .cloned()
            }),
        };
        let selection = select_model_profile(&overrides, default_profile)?;
        let profile = self.provider_registry.resolve_profile(&selection.profile)?;
        Ok((selection.profile, profile))
    }

    /// Runs the set model profile override operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_model_profile_override(
        &mut self,
        scope: RuntimeModelProfileOverrideScope,
        profile_name: &str,
    ) -> Result<()> {
        self.provider_registry.resolve_profile(profile_name)?;
        match scope {
            RuntimeModelProfileOverrideScope::Session => {
                self.model_profile_overrides.session_profile = Some(profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Window(window_id) => {
                self.model_profile_overrides
                    .window_profiles
                    .insert(window_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Pane(pane_id) => {
                self.model_profile_overrides
                    .pane_profiles
                    .insert(pane_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Agent(agent_id) => {
                self.model_profile_overrides
                    .agent_profiles
                    .insert(agent_id, profile_name.to_string());
            }
            RuntimeModelProfileOverrideScope::Subagent(agent_id) => {
                self.model_profile_overrides
                    .subagent_profiles
                    .insert(agent_id, profile_name.to_string());
            }
        }
        Ok(())
    }

    /// Runs the clear model profile override operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn clear_model_profile_override(&mut self, scope: RuntimeModelProfileOverrideScope) {
        match scope {
            RuntimeModelProfileOverrideScope::Session => {
                self.model_profile_overrides.session_profile = None;
            }
            RuntimeModelProfileOverrideScope::Window(window_id) => {
                self.model_profile_overrides
                    .window_profiles
                    .remove(&window_id);
            }
            RuntimeModelProfileOverrideScope::Pane(pane_id) => {
                self.model_profile_overrides.pane_profiles.remove(&pane_id);
            }
            RuntimeModelProfileOverrideScope::Agent(agent_id) => {
                self.model_profile_overrides
                    .agent_profiles
                    .remove(&agent_id);
            }
            RuntimeModelProfileOverrideScope::Subagent(agent_id) => {
                self.model_profile_overrides
                    .subagent_profiles
                    .remove(&agent_id);
            }
        }
    }

    /// Runs the inherited model profile for child agent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn inherited_model_profile_for_child_agent(
        &self,
        parent_agent_id: &str,
    ) -> Option<String> {
        if let Some(profile) = self
            .model_profile_overrides
            .agent_profiles
            .get(parent_agent_id)
        {
            return Some(profile.clone());
        }
        let parent_pane = parent_agent_id.strip_prefix("agent-")?;
        let default_profile = self.provider_registry.default_profile_name()?;
        self.active_model_profile_for_pane(parent_pane, parent_agent_id, None)
            .ok()
            .map(|(profile, _)| profile)
            .filter(|profile| profile != default_profile)
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

/// Borrowed argument view for `/model --secondary`.
///
/// The public parser lives in the runtime config module; this compact local
/// view keeps command execution readable without exposing parser internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeSecondaryModelCommandArgs<'a> {
    /// Optional requested model profile or provider model name.
    profile: Option<&'a str>,
    /// Optional reasoning profile or effort requested with the model.
    reasoning_profile: Option<&'a str>,
    /// Whether to reset the secondary model to the default router profile.
    clear: bool,
    /// Whether to list models for the active provider.
    list: bool,
    /// Whether to show the current secondary model.
    show: bool,
}

/// Carries Runtime Model Catalog state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RuntimeModelCatalog {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    provider: String,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    source: String,
    /// Stores the provider error value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    provider_error: Option<String>,
    /// Stores the models value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    models: Vec<ProviderModelInfo>,
    /// Stores the reasoning levels value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    reasoning_levels: Vec<String>,
    /// Provider-reported quota usage percentages from the catalog request.
    quota_usage: Vec<ProviderQuotaUsage>,
}

impl RuntimeModelCatalog {
    /// Runs the from provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_provider(catalog: ProviderModelCatalog) -> Self {
        Self {
            provider: catalog.provider,
            source: catalog.source,
            provider_error: None,
            models: catalog.models,
            reasoning_levels: dedupe_runtime_strings(catalog.reasoning_levels),
            quota_usage: catalog.quota_usage,
        }
    }
}

/// Runs the runtime configured model catalog operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_configured_model_catalog(
    provider_id: &str,
    provider_config: &crate::runtime::RuntimeProviderConfig,
    registry: &crate::runtime::RuntimeProviderRegistry,
) -> RuntimeModelCatalog {
    let mut models = BTreeMap::<String, ProviderModelInfo>::new();
    if let Some(default_model) = provider_config.default_model.as_deref()
        && !default_model.is_empty()
    {
        runtime_insert_catalog_model(
            &mut models,
            default_model,
            runtime_configured_reasoning_levels_for_model(provider_config, default_model),
        );
    }
    let configured_models = provider_config
        .models
        .iter()
        .map(String::as_str)
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    let default_models = if configured_models.is_empty() {
        runtime_default_models_for_provider(&provider_config.kind)
            .map(|models| models.to_vec())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let provider_models = if configured_models.is_empty() {
        default_models.as_slice()
    } else {
        configured_models.as_slice()
    };
    for model in provider_models {
        runtime_insert_catalog_model(
            &mut models,
            model,
            runtime_configured_reasoning_levels_for_model(provider_config, model),
        );
    }
    for profile in registry
        .profiles()
        .values()
        .filter(|profile| profile.provider == provider_id)
    {
        let mut reasoning_levels =
            runtime_configured_reasoning_levels_for_model(provider_config, &profile.model);
        if let Some(reasoning) = profile.reasoning_profile.as_deref() {
            reasoning_levels.push(reasoning.to_string());
        }
        runtime_insert_catalog_model(&mut models, &profile.model, reasoning_levels);
    }
    if models.is_empty()
        && let Ok(recommended_model) = runtime_recommended_model_for_provider(&provider_config.kind)
    {
        runtime_insert_catalog_model(
            &mut models,
            recommended_model,
            runtime_configured_reasoning_levels_for_model(provider_config, recommended_model),
        );
    }
    let models = models.into_values().collect::<Vec<_>>();
    let reasoning_levels = dedupe_runtime_strings(
        models
            .iter()
            .flat_map(|model| model.reasoning_levels.iter().cloned())
            .collect(),
    );
    RuntimeModelCatalog {
        provider: provider_id.to_string(),
        source: "config".to_string(),
        provider_error: None,
        models,
        reasoning_levels,
        quota_usage: Vec::new(),
    }
}

/// Runs the runtime insert catalog model operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_insert_catalog_model(
    models: &mut BTreeMap<String, ProviderModelInfo>,
    model: &str,
    reasoning_levels: Vec<String>,
) {
    let entry = models
        .entry(model.to_string())
        .or_insert_with(|| ProviderModelInfo {
            id: model.to_string(),
            display_name: None,
            reasoning_levels: Vec::new(),
        });
    entry.reasoning_levels.extend(
        reasoning_levels
            .into_iter()
            .filter(|level| !level.is_empty()),
    );
    entry.reasoning_levels = dedupe_runtime_strings(std::mem::take(&mut entry.reasoning_levels));
}

/// Runs the runtime configured reasoning levels for model operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_configured_reasoning_levels_for_model(
    provider_config: &crate::runtime::RuntimeProviderConfig,
    model: &str,
) -> Vec<String> {
    let mut levels = provider_config
        .options
        .get("reasoning_effort")
        .or_else(|| provider_config.options.get("reasoning_profile"))
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if provider_config.kind == "openai" {
        levels.extend(openai_default_reasoning_levels_for_model(model));
    }
    if provider_config.kind == "deepseek" {
        levels.extend(deepseek_default_reasoning_effort_levels());
    }
    dedupe_runtime_strings(levels)
}

/// Returns the reasoning effort levels supported by DeepSeek providers.
fn deepseek_default_reasoning_effort_levels() -> Vec<String> {
    vec!["high".to_string(), "max".to_string()]
}

/// Formats the current secondary auto-sizing model profile.
fn runtime_secondary_model_profile_display(
    secondary_name: &str,
    secondary_profile: &ModelProfile,
    active_profile: &ModelProfile,
) -> String {
    format!(
        "scope=secondary profile={} provider={} model={} reasoning_profile={} active_provider={} active_model={} source=runtime-secondary-model",
        json_escape(secondary_name),
        json_escape(&secondary_profile.provider),
        json_escape(&secondary_profile.model),
        secondary_profile
            .reasoning_profile
            .as_deref()
            .unwrap_or("none"),
        json_escape(&active_profile.provider),
        json_escape(&active_profile.model)
    )
}

/// Runs the runtime model catalog display operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_model_catalog_display(
    active_name: &str,
    active_profile: &ModelProfile,
    catalog: &RuntimeModelCatalog,
) -> String {
    let context_limit = format!("{} tokens", active_profile.context_window_tokens());
    let mut lines = vec!["## Model Catalog".to_string(), String::new()];
    if let Some(error) = catalog.provider_error.as_deref() {
        lines.push(format!(
            "**Provider catalog unavailable:** `{}`",
            runtime_model_catalog_unavailable_reason(error)
        ));
        lines.push(String::new());
    }
    if !catalog.reasoning_levels.is_empty() {
        lines.push(format!(
            "**Reasoning levels:** `{}`",
            catalog.reasoning_levels.join(", ")
        ));
        lines.push(String::new());
    }
    let model_rows = catalog
        .models
        .iter()
        .map(|model| {
            let model_name = runtime_model_catalog_model_name(model);
            let active_model =
                catalog.provider == active_profile.provider && model.id == active_profile.model;
            let model_name = if active_model {
                format!("★ {model_name}")
            } else {
                model_name
            };
            vec![
                catalog.provider.clone(),
                model_name,
                runtime_model_catalog_reasoning_display(
                    &model.reasoning_levels,
                    active_model.then_some(active_profile.reasoning_profile.as_deref()),
                ),
                context_limit.clone(),
                catalog.source.clone(),
                if active_model {
                    active_name.to_string()
                } else {
                    String::new()
                },
            ]
        })
        .collect::<Vec<_>>();
    if !model_rows.is_empty() {
        lines.extend(runtime_markdown_table(
            &[
                "Provider",
                "Model",
                "Reasoning levels",
                "Context limit",
                "Source",
                "Active profile",
            ],
            &model_rows,
        ));
    }
    lines.join("\n")
}

/// Formats a provider model name with optional display metadata.
fn runtime_model_catalog_model_name(model: &ProviderModelInfo) -> String {
    match model.display_name.as_deref() {
        Some(display_name) if !display_name.is_empty() => {
            format!("{} ({display_name})", model.id)
        }
        _ => model.id.clone(),
    }
}

/// Formats reasoning choices and marks the active reasoning level.
fn runtime_model_catalog_reasoning_display(
    levels: &[String],
    active_reasoning: Option<Option<&str>>,
) -> String {
    let mut values = if levels.is_empty() {
        vec!["default".to_string()]
    } else {
        levels.to_vec()
    };
    let active = active_reasoning.flatten().unwrap_or("default");
    if !values.iter().any(|level| level == active) {
        values.insert(0, active.to_string());
    }
    if active_reasoning.is_some() {
        values
            .into_iter()
            .map(|level| {
                if level == active {
                    format!("★ {level}")
                } else {
                    level
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        values.join(", ")
    }
}

/// Builds a plain markdown table from already formatted cell values.
fn runtime_markdown_table(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let header = headers
        .iter()
        .map(|cell| runtime_markdown_table_cell(cell))
        .collect::<Vec<_>>()
        .join(" | ");
    let separator = headers
        .iter()
        .map(|_| "---")
        .collect::<Vec<_>>()
        .join(" | ");
    let mut lines = vec![format!("| {header} |"), format!("| {separator} |")];
    lines.extend(rows.iter().map(|row| {
        let cells = row
            .iter()
            .map(|cell| runtime_markdown_table_cell(cell))
            .collect::<Vec<_>>()
            .join(" | ");
        format!("| {cells} |")
    }));
    lines
}

/// Escapes markdown table separators without changing the copyable value.
fn runtime_markdown_table_cell(value: &str) -> String {
    value.replace('|', r"\|").replace('\n', "<br>")
}

/// Runs the runtime model catalog unavailable reason operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_model_catalog_unavailable_reason(error: &str) -> String {
    if error.contains("ChatGPT browser credentials") {
        "browser-auth-catalog-unsupported".to_string()
    } else if error.contains("api.model.read") || error.contains("Missing scopes") {
        "missing-model-read-scope".to_string()
    } else if error.contains("attached auth store") || error.contains("authenticated provider") {
        "auth-unavailable".to_string()
    } else if error.contains("has not been refreshed") {
        "provider-info-not-refreshed".to_string()
    } else if error.contains("Models API returned status")
        || error.contains("model catalog")
        || error.contains("provider HTTP request failed")
    {
        "live-provider-catalog-unavailable".to_string()
    } else {
        error.replace(char::is_whitespace, "-")
    }
}

/// Runs the runtime generated model profile name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_generated_model_profile_name(
    registry: &crate::runtime::RuntimeProviderRegistry,
    provider_id: &str,
    model: &str,
    reasoning_profile: Option<&str>,
    profile: &ModelProfile,
) -> String {
    let base = match reasoning_profile {
        Some(reasoning) => format!("{model}:{reasoning}"),
        None => model.to_string(),
    };
    let preferred = match profile
        .latency_preference
        .as_deref()
        .filter(|latency| *latency != "default")
    {
        Some(latency) => format!("{base}:{latency}"),
        None => base,
    };
    if runtime_profile_name_available_or_matching(registry, &preferred, profile) {
        return preferred;
    }
    let mut candidate = format!("{provider_id}:{preferred}");
    if runtime_profile_name_available_or_matching(registry, &candidate, profile) {
        return candidate;
    }
    for index in 2usize.. {
        candidate = format!("{provider_id}:{preferred}:{index}");
        if runtime_profile_name_available_or_matching(registry, &candidate, profile) {
            return candidate;
        }
    }
    unreachable!("usize iteration should find an available generated model profile name")
}

/// Runs the runtime profile name available or matching operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_profile_name_available_or_matching(
    registry: &crate::runtime::RuntimeProviderRegistry,
    name: &str,
    profile: &ModelProfile,
) -> bool {
    registry.profile(name).is_none_or(|existing| {
        existing.provider == profile.provider
            && existing.model == profile.model
            && existing.reasoning_profile == profile.reasoning_profile
            && existing.latency_preference.as_deref().unwrap_or("default")
                == profile.latency_preference.as_deref().unwrap_or("default")
    })
}

/// Runs the dedupe runtime strings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn dedupe_runtime_strings(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !deduped.iter().any(|existing| existing == &value) {
            deduped.push(value);
        }
    }
    deduped
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

/// Builds the provider request used for model-authored conversation compaction.
fn runtime_model_compaction_request(
    profile: &ModelProfile,
    pane_id: &str,
    conversation_id: &str,
    transcript_entries: u64,
    entries: &[TranscriptEntry],
    context: &AgentContext,
) -> Result<ModelRequest> {
    let agent_id = format!("agent-{pane_id}");
    let turn_id = format!("compact-{conversation_id}");
    Ok(ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        reasoning_effort: profile
            .provider_options
            .get("reasoning_effort")
            .cloned()
            .or_else(|| profile.reasoning_profile.clone()),
        prompt_cache_retention: profile.provider_options.get("prompt_cache_retention").cloned(),
        latency_preference: profile.latency_preference.clone(),
        max_output_tokens: profile.max_output_tokens(),
        turn_id,
        agent_id,
        available_mcp_tools: Vec::new(),
        interaction_kind: ModelInteractionKind::ActionExecution,
        allowed_actions: AllowedActionSet::say_only(),
        messages: vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                content: "You are Mezzanine's conversation compactor. Produce durable, concise summaries that preserve task-critical context and omit secrets."
                    .to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::DeveloperInstruction,
                content: "Return exactly one `say` action with `status` set to `final` and `content_type` set to `text/markdown; charset=utf-8`. The text must summarize the conversation for a future agent turn. Preserve user goals, current plan, decisions, file paths, commands, test results, blockers, and pending follow-up. Do not claim work was completed unless the transcript proves it. Redact credentials and secrets."
                    .to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::Transcript,
                content: runtime_model_compaction_source(
                    pane_id,
                    conversation_id,
                    transcript_entries,
                    entries,
                    context,
                ),
            },
        ],
    })
}

/// Formats bounded transcript source material for a model compaction request.
fn runtime_model_compaction_source(
    pane_id: &str,
    conversation_id: &str,
    transcript_entries: u64,
    entries: &[TranscriptEntry],
    context: &AgentContext,
) -> String {
    let mut lines = vec![
        format!("Pane: {pane_id}"),
        format!("Conversation: {conversation_id}"),
        format!("Transcript entries before compaction: {transcript_entries}"),
        format!("Durable entries supplied for compaction: {}", entries.len()),
        format!(
            "Provider-bound context blocks supplied: {}",
            context.blocks.len()
        ),
    ];
    for (index, block) in context.blocks.iter().enumerate() {
        lines.push(format!(
            "context_block={} source={} label={} content={}",
            index,
            runtime_context_source_kind_name(block.source),
            json_escape(&block.label),
            runtime_model_compaction_entry_content(&block.content)
        ));
    }
    let selected = runtime_compact_selected_entries(entries);
    let omitted = entries.len().saturating_sub(selected.len());
    if omitted > 0 {
        lines.push(format!(
            "Middle durable transcript entries omitted from compaction source: {omitted}"
        ));
    }
    for entry in selected {
        lines.push(format!(
            "entry={} role={} turn={} pane={} content={}",
            entry.sequence,
            runtime_transcript_role_name(entry.role),
            entry.turn_id,
            entry.pane_id,
            runtime_model_compaction_entry_content(&entry.content)
        ));
    }
    lines.join("\n")
}

/// Returns a stable source label for context included in compaction input.
fn runtime_context_source_kind_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user-instruction",
        ContextSourceKind::DeveloperInstruction => "developer-instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::LocalMessage => "local-message",
        ContextSourceKind::ProjectGuidance => "project-guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript-user",
        ContextSourceKind::TranscriptAssistant => "transcript-assistant",
        ContextSourceKind::TranscriptTool => "transcript-tool",
        ContextSourceKind::ActionResult => "action-result",
    }
}

/// Bounds and redacts one transcript entry before sending it for compaction.
fn runtime_model_compaction_entry_content(content: &str) -> String {
    const MAX_MODEL_COMPACTION_ENTRY_BYTES: usize = 4096;
    let redacted = content
        .split_whitespace()
        .map(runtime_compact_redact_sensitive_token)
        .collect::<Vec<_>>()
        .join(" ");
    if redacted.len() <= MAX_MODEL_COMPACTION_ENTRY_BYTES {
        return redacted;
    }
    let mut end = MAX_MODEL_COMPACTION_ENTRY_BYTES;
    while !redacted.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}...[entry content elided before compaction; original_bytes={}]",
        &redacted[..end],
        redacted.len()
    )
}

/// Extracts the model-authored markdown summary from a compaction response.
fn runtime_model_compaction_summary_from_response(response: &ModelResponse) -> Result<String> {
    let summary = response
        .action_batch
        .as_ref()
        .and_then(|batch| {
            batch.actions.iter().find_map(|action| {
                if let AgentActionPayload::Say { text, .. } = &action.payload {
                    Some(text.trim().to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| response.raw_text.trim().to_string());
    if summary.trim().is_empty() {
        return Err(MezError::invalid_state(
            "model compaction response did not contain a summary",
        ));
    }
    Ok(summary)
}

/// Formats the durable memory record stored after model-authored compaction.
fn runtime_model_compact_memory_content(
    pane_id: &str,
    conversation_id: &str,
    transcript_entries: u64,
    summarized_entries: usize,
    model_profile_name: &str,
    profile: &ModelProfile,
    summary: &str,
) -> String {
    [
        format!("Model-generated compacted conversation summary for {conversation_id}."),
        format!("Pane: {pane_id}."),
        format!("Transcript entries before compaction: {transcript_entries}."),
        format!("Durable entries supplied to model: {summarized_entries}."),
        format!("Model profile: {model_profile_name}."),
        format!("Provider: {}.", profile.provider),
        format!("Model: {}.", profile.model),
        String::new(),
        summary.trim().to_string(),
    ]
    .join("\n")
}

/// Removes raw transcript replay blocks from the context supplied to the model
/// compactor so the retained tail is not summarized a second time.
///
/// # Parameters
/// - `context`: The provider context assembled for the compaction turn.
fn runtime_compaction_context_without_transcript_blocks(
    context: AgentContext,
) -> Result<AgentContext> {
    AgentContext::new(
        context
            .blocks
            .into_iter()
            .filter(|block| !runtime_context_block_is_transcript_replay(block))
            .collect(),
    )
}

/// Returns true when a context block is raw transcript replay.
///
/// # Parameters
/// - `block`: The context block being classified.
fn runtime_context_block_is_transcript_replay(block: &ContextBlock) -> bool {
    matches!(
        block.source,
        ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptUser
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
    )
}

/// Returns how many recent durable transcript entries should remain in raw
/// replay after a compaction summary is stored.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The durable transcript entries found for the
///   conversation.
/// - `context_budget_words`: The estimated context budget for the active model.
/// - `retained_tail_percent`: The model-context percentage reserved for raw replay.
fn runtime_compact_retained_transcript_entries(
    transcript_entries: u64,
    durable_entries: &[TranscriptEntry],
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> u64 {
    let active_count =
        runtime_compact_active_transcript_entry_count(transcript_entries, durable_entries.len());
    if active_count == 0 {
        return 0;
    }
    let active_entries = &durable_entries[durable_entries.len() - active_count..];
    let tail_budget = runtime_compact_retained_context_tail_budget_words(
        context_budget_words,
        retained_tail_percent,
    );
    let mut retained_entries = 0usize;
    let mut retained_words = 0usize;
    for entry in active_entries.iter().rev() {
        let entry_words = runtime_compact_transcript_entry_context_words(entry);
        if retained_entries > 0 && retained_words.saturating_add(entry_words) > tail_budget {
            break;
        }
        retained_entries += 1;
        retained_words = retained_words.saturating_add(entry_words);
    }
    u64::try_from(retained_entries).unwrap_or(u64::MAX)
}

/// Returns the raw tail count for an explicit user-forced compaction.
///
/// Manual `/compact` is an explicit request to compact, so it should not skip
/// only because all active entries currently fit inside the retained-tail
/// budget. Keep the normal budget-derived tail when it already leaves a prefix
/// to summarize, otherwise shrink the tail enough to summarize at least one
/// active durable entry.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The durable transcript entries found for the
///   conversation.
/// - `context_budget_words`: The estimated context budget for the active model.
/// - `retained_tail_percent`: The model-context percentage reserved for raw replay.
fn runtime_compact_forced_retained_transcript_entries(
    transcript_entries: u64,
    durable_entries: &[TranscriptEntry],
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> u64 {
    let retained = runtime_compact_retained_transcript_entries(
        transcript_entries,
        durable_entries,
        context_budget_words,
        retained_tail_percent,
    );
    let active_count =
        runtime_compact_active_transcript_entry_count(transcript_entries, durable_entries.len());
    if active_count == 0 {
        return 0;
    }
    let maximum_forced_retained = u64::try_from(active_count.saturating_sub(1)).unwrap_or(u64::MAX);
    retained.min(maximum_forced_retained)
}

/// Returns the active transcript entry count represented by the current shell
/// session and durable store.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The number of durable transcript entries found.
fn runtime_compact_active_transcript_entry_count(
    transcript_entries: u64,
    durable_entries: usize,
) -> usize {
    usize::try_from(transcript_entries)
        .unwrap_or(usize::MAX)
        .min(durable_entries)
}

/// Returns the durable transcript prefix that should be summarized, excluding
/// the exact raw tail retained for future turns.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The durable transcript entries found for the
///   conversation.
/// - `retained_transcript_entries`: The retained raw tail count.
fn runtime_compact_transcript_entries_for_summary(
    transcript_entries: u64,
    durable_entries: &[TranscriptEntry],
    retained_transcript_entries: u64,
) -> &[TranscriptEntry] {
    let active_count =
        runtime_compact_active_transcript_entry_count(transcript_entries, durable_entries.len());
    let retained_count = usize::try_from(retained_transcript_entries)
        .unwrap_or(usize::MAX)
        .min(active_count);
    let compactable_count = active_count.saturating_sub(retained_count);
    let active_start = durable_entries.len().saturating_sub(active_count);
    &durable_entries[active_start..active_start + compactable_count]
}

/// Returns the word budget reserved for retained exact transcript replay.
///
/// # Parameters
/// - `context_budget_words`: The estimated context budget for the active model.
/// - `retained_tail_percent`: The model-context percentage reserved for raw replay.
fn runtime_compact_retained_context_tail_budget_words(
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> usize {
    context_budget_words
        .saturating_mul(runtime_compact_retained_tail_percent(retained_tail_percent))
        .saturating_div(100)
        .max(1)
}

/// Normalizes retained-tail percentages for defensive runtime callers.
fn runtime_compact_retained_tail_percent(retained_tail_percent: usize) -> usize {
    retained_tail_percent.clamp(1, 100)
}

/// Estimates one transcript entry's provider-context footprint.
///
/// # Parameters
/// - `entry`: The transcript entry being estimated.
fn runtime_compact_transcript_entry_context_words(entry: &TranscriptEntry) -> usize {
    AGENT_COMPACT_TRANSCRIPT_ENTRY_CONTEXT_OVERHEAD_WORDS
        .saturating_add(model_context_text_word_count(&entry.content))
        .saturating_add(model_context_text_word_count(&entry.turn_id))
        .saturating_add(model_context_text_word_count(&entry.agent_id))
        .saturating_add(model_context_text_word_count(&entry.pane_id))
}

/// Runs the runtime compact selected entries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_compact_selected_entries(entries: &[TranscriptEntry]) -> Vec<&TranscriptEntry> {
    /// Defines the MAX COMPACTED TRANSCRIPT ENTRIES const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_COMPACTED_TRANSCRIPT_ENTRIES: usize = 12;
    /// Defines the LEADING COMPACTED TRANSCRIPT ENTRIES const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const LEADING_COMPACTED_TRANSCRIPT_ENTRIES: usize = 4;
    /// Defines the TRAILING COMPACTED TRANSCRIPT ENTRIES const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const TRAILING_COMPACTED_TRANSCRIPT_ENTRIES: usize = 8;
    if entries.len() <= MAX_COMPACTED_TRANSCRIPT_ENTRIES {
        return entries.iter().collect();
    }
    entries
        .iter()
        .take(LEADING_COMPACTED_TRANSCRIPT_ENTRIES)
        .chain(
            entries.iter().skip(
                entries
                    .len()
                    .saturating_sub(TRAILING_COMPACTED_TRANSCRIPT_ENTRIES),
            ),
        )
        .collect()
}

/// Runs the runtime compact redact sensitive token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_compact_redact_sensitive_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    if lower.contains("private")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower == "api"
        || lower.contains("password")
        || lower.contains("token")
        || lower.contains("credential")
        || lower.contains("secret")
        || token.contains("sk-")
        || token.contains('@') && token.contains('.')
    {
        "[redacted]".to_string()
    } else {
        token.to_string()
    }
}

/// Runs the runtime transcript role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_transcript_role_name(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::User => "user",
        TranscriptRole::Assistant => "assistant",
        TranscriptRole::Tool => "tool",
        TranscriptRole::System => "system",
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
            quota_usage: Default::default(),
            action_batch: Some(MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
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
        assert_eq!(request.allowed_actions.actions.len(), 1);
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
