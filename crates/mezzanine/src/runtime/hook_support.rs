//! Runtime Hook Support implementation.
//!
//! This module owns the runtime hook support boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AsyncMcpActionExecutor, AuditActor, AuditLog, AuditRecord, AuthStore, BTreeMap, ClientId,
    Command, DEFAULT_COMMAND_SHELL_CLASSIFICATION, Duration, EventKind, FocusedShellExecutor,
    FocusedShellHookOutput, HookEvent, HookExecutionPlan, HookExecutionResult, HookExecutionStatus,
    HookFailure, HookFailureKind, MarkerToken, McpActionExecutor, McpExecutionRequest,
    McpExecutionResponse, McpToolCallPlan, MezError, PaneDescriptor, Path,
    PendingFocusedShellHookContinuation, PendingFocusedShellHookTransaction, Read, Result,
    RuleDecision, RuntimeHookPipelineBlock, RuntimeMcpTransportSet, RuntimeSessionService, Stdio,
    current_unix_millis, exact_command_sha256, json_escape,
};
use mez_agent::{posix_shell_history_suppression_finish, posix_shell_history_suppression_start};
use wait_timeout::ChildExt;

// Runtime hook result, hook executor, and MCP executor support.

impl RuntimeHookPipelineBlock {
    /// Runs the from result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn from_result(result: &HookExecutionResult) -> Self {
        let failure = result.failure.as_ref();
        Self {
            hook_id: result.hook_id.clone(),
            event: result.event,
            failure_kind: failure
                .map(|failure| failure.kind)
                .unwrap_or(HookFailureKind::Planning),
            message: failure
                .map(|failure| failure.message.clone())
                .unwrap_or_else(|| "hook blocked action".to_string()),
        }
    }

    /// Runs the structured json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn structured_json(&self) -> String {
        format!(
            r#"{{"hook_blocked":{{"hook_id":"{}","event":"{}","failure_kind":"{:?}","message":"{}"}}}}"#,
            json_escape(&self.hook_id),
            runtime_hook_event_name(self.event),
            self.failure_kind,
            json_escape(&self.message)
        )
    }
}

/// Runs the runtime hook event for lifecycle operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_event_for_lifecycle(
    kind: EventKind,
    payload: &str,
) -> Option<HookEvent> {
    match kind {
        EventKind::ClientAttached => Some(HookEvent::ClientAttach),
        EventKind::ClientDetached => Some(HookEvent::ClientDetach),
        EventKind::PaneChanged if payload.contains(r#""process_state":"running""#) => {
            Some(HookEvent::PaneCreate)
        }
        EventKind::PaneChanged
            if payload.contains(r#""process_state":"exited""#)
                || payload.contains(r#""closed":true"#) =>
        {
            Some(HookEvent::PaneClose)
        }
        EventKind::WindowChanged if payload.contains(r#""closed":true"#) => {
            Some(HookEvent::WindowClose)
        }
        EventKind::WindowChanged if payload.contains(r#""state":"created"#) => {
            Some(HookEvent::WindowCreate)
        }
        EventKind::SnapshotChanged if payload.contains("snapshot_restore") => {
            Some(HookEvent::LayoutLoad)
        }
        EventKind::SnapshotChanged => Some(HookEvent::LayoutSave),
        EventKind::AgentStatus if payload.contains(r#""turn_started""#) => {
            Some(HookEvent::AgentTurnStart)
        }
        EventKind::AgentStatus
            if payload.contains(r#""state":"completed""#)
                || payload.contains(r#""state":"failed""#)
                || payload.contains(r#""state":"cancelled""#) =>
        {
            Some(HookEvent::AgentTurnStop)
        }
        _ => None,
    }
}

/// Runs the runtime hook event name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_hook_event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart => "session_start",
        HookEvent::SessionStop => "session_stop",
        HookEvent::ClientAttach => "client_attach",
        HookEvent::ClientDetach => "client_detach",
        HookEvent::WindowCreate => "window_create",
        HookEvent::WindowClose => "window_close",
        HookEvent::SessionDetach => "session_detach",
        HookEvent::PaneCreate => "pane_create",
        HookEvent::PaneClose => "pane_close",
        HookEvent::UserPromptSubmit => "user_prompt_submit",
        HookEvent::AgentTurnStart => "agent_turn_start",
        HookEvent::AgentTurnStop => "agent_turn_stop",
        HookEvent::PreShellCommand => "pre_shell_command",
        HookEvent::PostShellCommand => "post_shell_command",
        HookEvent::PermissionRequest => "permission_request",
        HookEvent::PermissionDecision => "permission_decision",
        HookEvent::PreMcpToolUse => "pre_mcp_tool_use",
        HookEvent::PostMcpToolUse => "post_mcp_tool_use",
        HookEvent::LayoutSave => "layout_save",
        HookEvent::LayoutLoad => "layout_load",
    }
}

/// Carries Runtime Focused Shell Pane Executor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct RuntimeFocusedShellPaneExecutor<'a> {
    /// Stores the service value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) service: &'a mut RuntimeSessionService,
    /// Stores the primary client id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) primary_client_id: ClientId,
    /// Stores the continuation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) continuation: Option<PendingFocusedShellHookContinuation>,
}

/// Carries Runtime Mcp Action Executor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct RuntimeMcpActionExecutor<'a> {
    /// Product-owned MCP transport connections.
    pub(super) transports: &'a mut RuntimeMcpTransportSet,
    /// Optional product audit sink for the tool call.
    pub(super) audit_log: Option<&'a mut AuditLog>,
    /// Environment supplied to the product transport.
    pub(super) environment: BTreeMap<String, String>,
    /// Optional product credential source for authenticated transports.
    pub(super) auth_store: Option<&'a AuthStore>,
    /// Session identity recorded in audit events.
    pub(super) session_id: String,
    /// Actor identity recorded in audit events.
    pub(super) actor: AuditActor,
    /// Stable call identity recorded in audit events.
    pub(super) call_id: String,
    /// Approved product plan retaining audit, approval, and effect policy.
    pub(super) plan: &'a McpToolCallPlan,
}

impl RuntimeMcpActionExecutor<'_> {
    /// Confirms the agent request still matches the approved product plan.
    fn validate_request(&self, request: &McpExecutionRequest) -> Result<()> {
        if self.plan.server_id != request.server_id
            || self.plan.tool_name != request.tool_name
            || self.plan.arguments_json.trim() != request.arguments_json.trim()
            || self.plan.timeout_ms != request.timeout_ms
        {
            return Err(MezError::invalid_args(
                "MCP execution request does not match the approved product plan",
            ));
        }
        Ok(())
    }
}

impl McpActionExecutor for RuntimeMcpActionExecutor<'_> {
    type Error = MezError;

    /// Executes one approved MCP request through the product transport.
    fn execute_mcp_call(&mut self, request: &McpExecutionRequest) -> Result<McpExecutionResponse> {
        self.validate_request(request)?;
        if let Some(audit_log) = self.audit_log.as_mut() {
            audit_log.append(AuditRecord::mcp_call(
                &self.session_id,
                self.actor.clone(),
                &request.server_id,
                &request.tool_name,
                &self.call_id,
                &request.arguments_json,
                "started",
            ))?;
        }
        let result = self.transports.call_tool(self.plan, &self.environment);
        let outcome = match &result {
            Ok(response) if response.is_error => "tool_error",
            Ok(_) => "succeeded",
            Err(_) => "failed",
        };
        if let Some(audit_log) = self.audit_log.as_mut() {
            audit_log.append(AuditRecord::mcp_call(
                &self.session_id,
                self.actor.clone(),
                &request.server_id,
                &request.tool_name,
                &self.call_id,
                &request.arguments_json,
                outcome,
            ))?;
        }
        result.map(Into::into)
    }
}

impl AsyncMcpActionExecutor for RuntimeMcpActionExecutor<'_> {
    type Error = MezError;

    /// Executes one approved MCP request asynchronously through the product transport.
    async fn execute_mcp_call_async(
        &mut self,
        request: &McpExecutionRequest,
    ) -> Result<McpExecutionResponse> {
        self.validate_request(request)?;
        if let Some(audit_log) = self.audit_log.as_mut() {
            audit_log.append(AuditRecord::mcp_call(
                &self.session_id,
                self.actor.clone(),
                &request.server_id,
                &request.tool_name,
                &self.call_id,
                &request.arguments_json,
                "started",
            ))?;
        }
        let result = self
            .transports
            .call_tool_async(self.plan, &self.environment, self.auth_store)
            .await;
        let outcome = match &result {
            Ok(response) if response.is_error => "tool_error",
            Ok(_) => "succeeded",
            Err(_) => "failed",
        };
        if let Some(audit_log) = self.audit_log.as_mut() {
            audit_log.append(AuditRecord::mcp_call(
                &self.session_id,
                self.actor.clone(),
                &request.server_id,
                &request.tool_name,
                &self.call_id,
                &request.arguments_json,
                outcome,
            ))?;
        }
        result.map(Into::into)
    }
}

impl FocusedShellExecutor for RuntimeFocusedShellPaneExecutor<'_> {
    /// Runs the run hook command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn run_hook_command(&mut self, plan: &HookExecutionPlan) -> Result<FocusedShellHookOutput> {
        let shell_command = plan
            .shell_command
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("focused-shell hook plan is missing command"))?;
        match self
            .service
            .permission_policy()
            .evaluate_shell_command_with_approvals(shell_command, self.service.session_approvals())
        {
            RuleDecision::Allow => {}
            RuleDecision::Prompt => {
                return Ok(focused_shell_policy_denied_output(
                    "focused-shell hook command requires approval",
                ));
            }
            RuleDecision::Forbid => {
                return Ok(focused_shell_policy_denied_output(
                    "focused-shell hook command is forbidden by permission policy",
                ));
            }
        }
        if !self.service.focused_shell_available_for_plan(Some(plan)) {
            if !plan.blocks_on_shell_availability {
                return run_external_shell_hook_command(self.service.session.shell.path(), plan);
            }
            return Ok(focused_shell_unavailable_output());
        }
        let descriptor = runtime_focused_shell_descriptor_for_plan(self.service, plan)?;
        let marker_sequence = self
            .service
            .integration
            .allocate_focused_shell_hook_marker();
        let marker = MarkerToken::new(exact_command_sha256(
            DEFAULT_COMMAND_SHELL_CLASSIFICATION,
            &format!(
                "focused-shell-hook\0{}\0{}\0{}\0{}",
                marker_sequence, descriptor.pane_id, plan.hook_id, shell_command
            ),
        ))?;
        let input = format!(
            "{history_start}\
MEZ_HOOK_PAYLOAD={payload}\n\
MEZ_MARKER_TOKEN={marker}\n\
MEZ_TURN={turn}\n\
MEZ_AGENT='focused-shell-hook'\n\
MEZ_PANE={pane}\n\
printf '\\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
\"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\n\
{command}\n\
MEZ_STATUS=$?\n\
printf '\\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\\033\\\\' \
\"$MEZ_STATUS\" \"$MEZ_MARKER_TOKEN\" \"$MEZ_TURN\" \"$MEZ_AGENT\" \"$MEZ_PANE\"\n\
unset MEZ_HOOK_PAYLOAD MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS\n\
{history_finish}",
            history_start = posix_shell_history_suppression_start(),
            history_finish = posix_shell_history_suppression_finish(),
            payload = shell_single_quote(&plan.event_payload_json),
            marker = shell_single_quote(marker.as_str()),
            turn = shell_single_quote(&format!("hook:{}", plan.hook_id)),
            pane = shell_single_quote(descriptor.pane_id.as_str()),
            command = shell_command
        );
        match self.service.write_input_to_pane_descriptor(
            &self.primary_client_id,
            &descriptor,
            input.as_bytes(),
        ) {
            Ok(_) => {
                self.service
                    .integration
                    .focused_shell_hook_transactions_mut()
                    .insert(
                        marker.as_str().to_string(),
                        PendingFocusedShellHookTransaction {
                            pane_id: descriptor.pane_id.to_string(),
                            plan: plan.clone(),
                            started_at_unix_ms: current_unix_millis(),
                            timeout_ms: plan.timeout_ms,
                            continuation: self.continuation.clone(),
                        },
                    );
                Ok(FocusedShellHookOutput {
                    exit_code: None,
                    stdout: "focused-shell hook queued in active pane".to_string(),
                    stderr: String::new(),
                    timed_out: false,
                    shell_unavailable: false,
                    policy_denied: false,
                })
            }
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => {
                Ok(focused_shell_unavailable_output())
            }
            Err(error) => Err(error),
        }
    }
}

/// Runs the runtime focused shell descriptor for plan operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_focused_shell_descriptor_for_plan(
    service: &RuntimeSessionService,
    plan: &HookExecutionPlan,
) -> Result<PaneDescriptor> {
    if let Some(target_pane_id) = plan.target_pane_id.as_deref() {
        return service
            .find_pane_descriptor(target_pane_id)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "pane not found"));
    }
    service.active_window_pane_descriptor(None)
}

/// Runs the focused shell unavailable output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn focused_shell_unavailable_output() -> FocusedShellHookOutput {
    FocusedShellHookOutput {
        exit_code: None,
        stdout: String::new(),
        stderr: "focused shell is unavailable".to_string(),
        timed_out: false,
        shell_unavailable: true,
        policy_denied: false,
    }
}

/// Runs the run external shell hook command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_external_shell_hook_command(
    shell_path: &Path,
    plan: &HookExecutionPlan,
) -> Result<FocusedShellHookOutput> {
    let shell_command = plan
        .shell_command
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("focused-shell hook plan is missing command"))?;
    let mut child = Command::new(shell_path)
        .arg("-lc")
        .arg(shell_command)
        .env("MEZ_HOOK_PAYLOAD", &plan.event_payload_json)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!(
                    "failed to spawn external shell hook `{}`: {error}",
                    plan.hook_id
                ),
            )
        })?;
    let Some(status) = child.wait_timeout(Duration::from_millis(plan.timeout_ms))? else {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(FocusedShellHookOutput {
            exit_code: None,
            stdout: read_runtime_child_pipe(child.stdout.take())?,
            stderr: read_runtime_child_pipe(child.stderr.take())?,
            timed_out: true,
            shell_unavailable: false,
            policy_denied: false,
        });
    };
    Ok(FocusedShellHookOutput {
        exit_code: status.code(),
        stdout: read_runtime_child_pipe(child.stdout.take())?,
        stderr: read_runtime_child_pipe(child.stderr.take())?,
        timed_out: false,
        shell_unavailable: false,
        policy_denied: false,
    })
}

/// Runs the read runtime child pipe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn read_runtime_child_pipe<T: Read>(pipe: Option<T>) -> Result<String> {
    let Some(mut pipe) = pipe else {
        return Ok(String::new());
    };
    let mut output = String::new();
    pipe.read_to_string(&mut output)?;
    Ok(output)
}

/// Runs the focused shell pre action failed result operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn focused_shell_pre_action_failed_result(
    plan: &HookExecutionPlan,
    kind: HookFailureKind,
    message: &str,
    retryable: bool,
) -> HookExecutionResult {
    HookExecutionResult {
        hook_id: plan.hook_id.clone(),
        event: plan.event,
        status: HookExecutionStatus::Failed,
        exit_code: None,
        stdout: String::new(),
        stderr: message.to_string(),
        failure: Some(HookFailure {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            kind,
            message: message.to_string(),
            retryable,
        }),
    }
}

/// Runs the focused shell pre action timeout result operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn focused_shell_pre_action_timeout_result(
    plan: &HookExecutionPlan,
) -> HookExecutionResult {
    HookExecutionResult {
        hook_id: plan.hook_id.clone(),
        event: plan.event,
        status: HookExecutionStatus::TimedOut,
        exit_code: None,
        stdout: String::new(),
        stderr: "focused-shell pre-action hook timed out".to_string(),
        failure: Some(HookFailure {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            kind: HookFailureKind::Timeout,
            message: "focused-shell pre-action hook timed out".to_string(),
            retryable: true,
        }),
    }
}

/// Runs the focused shell policy denied output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn focused_shell_policy_denied_output(message: &str) -> FocusedShellHookOutput {
    FocusedShellHookOutput {
        exit_code: Some(126),
        stdout: String::new(),
        stderr: message.to_string(),
        timed_out: false,
        shell_unavailable: false,
        policy_denied: true,
    }
}

/// Runs the shell single quote operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn shell_single_quote(value: &str) -> String {
    let mut quoted = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}
