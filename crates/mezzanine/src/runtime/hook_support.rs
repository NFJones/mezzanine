//! Runtime Hook Support implementation.
//!
//! This module owns the runtime hook support boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AsyncMcpActionExecutor, AuditActor, AuditLog, AuditRecord, AuthStore, BTreeMap, Command,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, Duration, EventKind, FocusedShellExecutor,
    FocusedShellHookOutput, HookEvent, HookExecutionPlan, HookExecutionResult, HookExecutionStatus,
    HookFailure, HookFailureKind, MarkerToken, McpActionExecutor, McpExecutionRequest,
    McpExecutionResponse, McpToolCallPlan, MezError, PaneDescriptor, Path,
    PendingFocusedShellHookContinuation, PendingFocusedShellHookTransaction, Read, Result,
    RuleDecision, RuntimeHookPipelineBlock, RuntimeMcpTransportSet, RuntimeSessionService,
    ShellTransaction, Stdio, current_unix_millis, exact_command_sha256, json_escape,
};
use crate::host::process::wait_for_child_with_timeout;

/// Maximum bytes retained independently from each external-shell hook stream.
const EXTERNAL_SHELL_HOOK_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;

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
#[allow(
    dead_code,
    reason = "retained for direct synchronous and async MCP service adapters"
)]
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
        let permission_policy = plan
            .target_pane_id
            .as_deref()
            .map(|pane_id| self.service.permission_policy_for_pane(pane_id))
            .unwrap_or_else(|| self.service.permission_policy().clone());
        match permission_policy
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
        let hook_command = format!(
            "MEZ_HOOK_PAYLOAD={payload}\n\
{{\n\
{command}\n\
}}\n\
MEZ_STATUS=$?\n\
unset MEZ_HOOK_PAYLOAD\n\
(exit \"$MEZ_STATUS\")",
            payload = shell_single_quote(&plan.event_payload_json),
            command = shell_command
        );
        let shell_identity = self
            .service
            .shell_execution_identity_for_pane(descriptor.pane_id.as_str())?;
        let classification = shell_identity.classification();
        let transaction = self.service.configure_shell_transaction_for_pane(
            descriptor.pane_id.as_str(),
            ShellTransaction::new(
                marker.clone(),
                format!("hook:{}", plan.hook_id),
                "focused-shell-hook",
                descriptor.pane_id.as_str(),
                shell_identity.shell_path(),
                hook_command,
            )?,
        );
        let input = transaction.render_stateful_for_classification_input(classification);
        self.service.require_generated_shell_input(&input)?;
        let mut wrapper = input.wrapper.clone();
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        let marker_id = marker.as_str().to_string();
        let receiver_payload = (!input.receiver_payload.is_empty()).then(|| {
            mez_mux::process::ShellInputDelivery::receiver_acknowledged(
                input.receiver_payload.into_bytes(),
                marker_id.clone(),
                true,
            )
        });
        self.service.register_running_shell_transaction(
            marker_id.clone(),
            crate::runtime::RunningShellTransactionRef {
                turn_id: format!("hook:{}", plan.hook_id),
                kind: crate::runtime::RunningShellTransactionKind::FocusedShellHook,
                pane_id: descriptor.pane_id.to_string(),
                command: shell_command.to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: None,
                pending_input_payload: None,
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
            true,
        );
        if let Some(receiver_payload) = receiver_payload {
            self.service
                .register_shell_receiver_payload(&marker_id, receiver_payload);
        }
        self.service
            .integration
            .focused_shell_hook_transactions_mut()
            .insert(
                marker_id.clone(),
                PendingFocusedShellHookTransaction {
                    pane_id: descriptor.pane_id.to_string(),
                    plan: plan.clone(),
                    started_at_unix_ms: current_unix_millis(),
                    timeout_ms: plan.timeout_ms,
                    continuation: self.continuation.clone(),
                },
            );
        match self
            .service
            .write_runtime_pane_shell_input(descriptor.pane_id.as_str(), wrapper.as_bytes())
        {
            Ok(_) => Ok(FocusedShellHookOutput {
                exit_code: None,
                stdout: "focused-shell hook queued in active pane".to_string(),
                stderr: String::new(),
                stdout_bytes: "focused-shell hook queued in active pane".len(),
                stderr_bytes: 0,
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                shell_unavailable: false,
                policy_denied: false,
            }),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => {
                self.service.remove_running_shell_transaction(&marker_id);
                self.service
                    .clear_shell_transaction_protocol_state(&marker_id);
                self.service
                    .integration
                    .focused_shell_hook_transactions_mut()
                    .remove(&marker_id);
                Ok(focused_shell_unavailable_output())
            }
            Err(error) => {
                self.service.remove_running_shell_transaction(&marker_id);
                self.service
                    .clear_shell_transaction_protocol_state(&marker_id);
                self.service
                    .integration
                    .focused_shell_hook_transactions_mut()
                    .remove(&marker_id);
                Err(error)
            }
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
        stdout_bytes: 0,
        stderr_bytes: "focused shell is unavailable".len(),
        stdout_truncated: false,
        stderr_truncated: false,
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
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_runtime_child_pipe(stdout));
    let stderr_reader = std::thread::spawn(move || read_runtime_child_pipe(stderr));
    let status = wait_for_child_with_timeout(&mut child, Duration::from_millis(plan.timeout_ms))?;
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let stdout = join_runtime_child_pipe_reader(stdout_reader)?;
    let stderr = join_runtime_child_pipe_reader(stderr_reader)?;
    let Some(status) = status else {
        return Ok(FocusedShellHookOutput {
            exit_code: None,
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: stdout.observed_bytes,
            stderr_bytes: stderr.observed_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            timed_out: true,
            shell_unavailable: false,
            policy_denied: false,
        });
    };
    Ok(FocusedShellHookOutput {
        exit_code: status.code(),
        stdout: stdout.text,
        stderr: stderr.text,
        stdout_bytes: stdout.observed_bytes,
        stderr_bytes: stderr.observed_bytes,
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
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
fn read_runtime_child_pipe<T: Read>(pipe: Option<T>) -> Result<RuntimeBoundedHookOutput> {
    let Some(mut pipe) = pipe else {
        return Ok(RuntimeBoundedHookOutput::default());
    };
    let mut retained = Vec::new();
    let mut observed_bytes = 0usize;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(read);
        let remaining = EXTERNAL_SHELL_HOOK_OUTPUT_LIMIT_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    runtime_bounded_hook_output(retained, observed_bytes)
}

/// Joins one external-shell pipe reader without allowing a panic to escape.
fn join_runtime_child_pipe_reader(
    reader: std::thread::JoinHandle<Result<RuntimeBoundedHookOutput>>,
) -> Result<RuntimeBoundedHookOutput> {
    reader
        .join()
        .map_err(|_| MezError::invalid_state("external-shell hook pipe reader thread panicked"))?
}

/// Bounded retained external-shell output plus complete byte accounting.
#[derive(Debug, Default)]
struct RuntimeBoundedHookOutput {
    text: String,
    observed_bytes: usize,
    truncated: bool,
}

/// Converts one retained external-shell prefix without splitting UTF-8.
fn runtime_bounded_hook_output(
    mut retained: Vec<u8>,
    observed_bytes: usize,
) -> Result<RuntimeBoundedHookOutput> {
    let truncated = observed_bytes > retained.len();
    match String::from_utf8(retained) {
        Ok(text) => Ok(RuntimeBoundedHookOutput {
            text,
            observed_bytes,
            truncated,
        }),
        Err(error) if error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            retained = error.into_bytes();
            retained.truncate(valid_up_to);
            let text = String::from_utf8(retained).map_err(|error| {
                MezError::new(
                    crate::error::MezErrorKind::Io,
                    format!("hook output is not UTF-8: {error}"),
                )
            })?;
            Ok(RuntimeBoundedHookOutput {
                text,
                observed_bytes,
                truncated: true,
            })
        }
        Err(error) => Err(MezError::new(
            crate::error::MezErrorKind::Io,
            format!("hook output is not UTF-8: {error}"),
        )),
    }
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
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: false,
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
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: false,
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
        stdout_bytes: 0,
        stderr_bytes: message.len(),
        stdout_truncated: false,
        stderr_truncated: false,
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
