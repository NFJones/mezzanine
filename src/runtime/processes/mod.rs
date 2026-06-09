//! Runtime Processes implementation.
//!
//! This module owns the runtime processes boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod layout;
mod output_filter;
mod startup;
mod transactions;

use layout::terminal_clipboard_policy_accepts_osc52;

use super::{
    ActionContentBlock, ActionResult, ActionStatus, ActivePanePipe, AgentId, AgentTurnRecord,
    AgentTurnState, AuditActor, BTreeSet, ContextBlock, ContextSourceKind, DeferredPaneInput,
    DeferredPanePipeWrite, DeferredPaneResize, DeferredPaneTermination, EventKind,
    ExitedPaneProcess, HookEvent, HookExecutionResult, HookExecutionStatus, HookFailure,
    HookFailureKind, MezError, PaneDescriptor, PaneExitRecord, PaneExitStatus, PaneExitUpdate,
    PaneId, PaneOutputUpdate, PaneProcessOutput, PaneProcessStart, PaneReadinessState,
    PaneResizeUpdate, PaneSizeSpec, Path, PathBuf, ReadinessOverrideRevocation, ResizeAxis,
    ResizeDirection, Result, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeHookPipelineBlock, RuntimeLifecycleState, RuntimeSessionService,
    RuntimeShellTransactionActionFailure, RuntimeShellTransactionTimerKind,
    RuntimeShellTransactionTimerRef, SessionSnapshotPayload, ShellClassification, ShellTransaction,
    Size, SplitDirection, StoppedPanePipe, TerminalOscEvent, TerminalScreen, WindowId,
    action_result_context_content, current_unix_millis, current_unix_seconds,
    decode_shell_output_transport_with_diagnostics, focused_shell_pre_action_timeout_result,
    hook_execution_audit_record, json_escape, local_action_plan, optional_i32_json,
    pane_content_size_for_geometry, pane_environment_with_term,
    postprocess_shell_action_success_output, rendered_window_body_size,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_hook_event_name,
    runtime_hook_execution_status_name, runtime_marker_for_action,
    runtime_pane_readiness_state_name, runtime_post_shell_hook_payload,
    runtime_random_marker_token, shell_command_result_content,
    shell_command_structured_content_json, validate_pane_size_for_resize,
};
use crate::agent::{
    AgentActionPayload, ApplyPatchTransactionPhase, DEFAULT_BOOTSTRAP_TIMEOUT_MS,
    apply_patch_transaction_phase, bootstrap_script_for_classification, parse_bootstrap_env_output,
    readiness_probe_command_for_classification,
};
use crate::process::PaneProcess;
use crate::terminal::{TerminalStyledLine, parse_mez_shell_transaction_osc};

use output_filter::*;
use transactions::*;

// Pane process lifecycle and PTY synchronization.

/// Defines the RUNTIME FOREGROUND TITLE IDLE SYNC POLL INTERVAL const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL: usize = 16;
/// Defines the RUNTIME READINESS PROBE TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_READINESS_PROBE_TIMEOUT_MS: u64 = 5_000;
/// Maximum time a transaction may wait for its payload receiver start marker.
///
/// Non-stateful agent actions stream the command body only after the shell
/// wrapper emits an OSC start marker. If that marker is lost or the wrapper is
/// stranded before the receiver loop, waiting for the full command timeout makes
/// the pane look hung even though no user command has actually started.
const RUNTIME_SHELL_TRANSACTION_START_TIMEOUT_MS: u64 = 30_000;

/// Runs the runtime running shell transaction kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_running_shell_transaction_kind_name(kind: &RunningShellTransactionKind) -> &'static str {
    match kind {
        RunningShellTransactionKind::AgentAction { .. } => "agent_action",
        RunningShellTransactionKind::ReadinessProbe => "readiness_probe",
        RunningShellTransactionKind::Bootstrap => "bootstrap",
    }
}

/// Returns the next runtime timeout deadline for one shell transaction.
///
/// Transactions with deferred payloads have an additional short start deadline:
/// the shell must reach the receiver loop and emit its start marker before the
/// command body is sent. Once that happens the pending payload is cleared and
/// the ordinary command timeout applies.
fn runtime_shell_transaction_effective_timeout_ms(
    transaction: &RunningShellTransactionRef,
) -> Option<u64> {
    let timeout_ms = transaction.timeout_ms?;
    if transaction.pending_input_payload.is_some() {
        Some(timeout_ms.min(RUNTIME_SHELL_TRANSACTION_START_TIMEOUT_MS))
    } else {
        Some(timeout_ms)
    }
}

/// Builds structured terminal observation data for a shell protocol violation.
fn shell_transaction_protocol_violation_observation(
    marker: &str,
    transaction: &RunningShellTransactionRef,
    boundary_state: &str,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "source": "pty",
        "stream": "pty_combined",
        "marker": marker,
        "exit_code": null,
        "signal": null,
        "timed_out": false,
        "combined_output_bytes": transaction.observed_output_bytes,
        "combined_output_preview": transaction.observed_output_preview,
        "boundary_state": boundary_state,
        "output_truncated": transaction.observed_output_truncated,
        "protocol_violation": true,
        "protocol_violation_message": message
    })
}

/// Builds model-facing terminal evidence for a pane input write failure.
fn pane_write_failure_terminal_observation(
    marker: &str,
    transaction: &RunningShellTransactionRef,
    boundary_state: &str,
    error: &str,
) -> serde_json::Value {
    serde_json::json!({
        "source": "pty",
        "stream": "pty_input",
        "marker": marker,
        "exit_code": null,
        "signal": null,
        "timed_out": false,
        "error": error,
        "combined_output_bytes": transaction.observed_output_bytes,
        "combined_output_preview": transaction.observed_output_preview,
        "boundary_state": boundary_state,
        "output_truncated": transaction.observed_output_truncated
    })
}

impl RuntimeSessionService {
    /// Runs the shell classification for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn shell_classification_for_pane(&self, pane_id: &str) -> ShellClassification {
        self.pane_environment_signatures
            .get(pane_id)
            .map(|signature| signature.shell_classification)
            .unwrap_or_else(|| ShellClassification::classify(self.session.shell.path()))
    }

    /// Runs the poll pane processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn poll_pane_processes(&mut self) -> Result<Vec<PaneExitUpdate>> {
        self.require_live()?;
        let exited = self.pane_processes.poll_exited()?;
        let mut updates = Vec::new();

        for process in exited {
            updates.push(self.apply_exited_pane_process(process, true)?);
        }

        Ok(updates)
    }

    /// Applies one pane process exit event delivered by an async process watcher.
    ///
    /// The live polling path still removes recorded exits from
    /// `PaneProcessManager`; event-driven callers may not keep ownership there,
    /// so this method applies the session, event-log, registry, pane-pipe, and
    /// agent-turn cleanup without assuming the synchronous manager observed the
    /// exit first.
    pub fn apply_pane_process_exit_event(
        &mut self,
        pane_id: impl Into<String>,
        primary_pid: u32,
        exit_status: PaneExitStatus,
    ) -> Result<Option<PaneExitUpdate>> {
        self.require_live()?;
        let pane_id = pane_id.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Ok(None);
        }
        let primary_pid = if primary_pid == 0 {
            self.primary_pid_for_live_pane_process(&pane_id)
                .unwrap_or(0)
        } else {
            primary_pid
        };
        self.apply_exited_pane_process(
            ExitedPaneProcess {
                pane_id: pane_id.clone(),
                primary_pid,
                status: exit_status,
            },
            false,
        )
        .map(|update| {
            self.async_owned_pane_processes.remove(&pane_id);
            Some(update)
        })
    }

    /// Applies one process lifecycle failure delivered by an async watcher.
    ///
    /// This records a diagnostic pane event without closing the pane. The
    /// process may still be live when a watcher, wait task, resize operation, or
    /// write bridge fails, so lifecycle mutation remains the responsibility of a
    /// later explicit exit or termination event.
    pub fn apply_pane_process_failure_event(
        &mut self,
        pane_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let error = error.into();
        let primary_pid = self
            .primary_pid_for_live_pane_process(descriptor.pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"failed","error":"{}"}}"#,
                json_escape(descriptor.pane_id.as_str()),
                json_escape(descriptor.window_id.as_str()),
                primary_pid,
                json_escape(&error)
            ),
        )?;
        Ok(true)
    }

    /// Applies one process-spawn lifecycle event delivered by an async watcher.
    ///
    /// This is the event-driven equivalent of the post-spawn bookkeeping in
    /// `start_pane_process_with_start_directory`: it refreshes the pane's
    /// terminal screen state, marks readiness as unknown, queues bootstrap
    /// observation, and emits the replayable pane-start lifecycle event. The
    /// async process owner is responsible for retaining the live process handle.
    pub fn apply_pane_process_spawn_event(
        &mut self,
        pane_id: impl Into<String>,
        pid: Option<u32>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let primary_pid = pid
            .or_else(|| self.pane_processes.primary_pid(descriptor.pane_id.as_str()))
            .unwrap_or(0);
        self.pane_exit_records.remove(descriptor.pane_id.as_str());
        self.session
            .set_pane_live_state(descriptor.pane_id.as_str(), true)?;
        self.pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.pane_bootstrap_pending
            .insert(descriptor.pane_id.to_string());

        let update = PaneProcessStart {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            size: descriptor.size,
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_start_event(&update)?;
        Ok(true)
    }

    /// Applies one pane input write failure delivered by an async pane driver.
    pub fn apply_pane_write_failure_event(
        &mut self,
        pane_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        let error = error.into();
        let pane_id = descriptor.pane_id.to_string();
        let window_id = descriptor.window_id.to_string();
        let primary_pid = self
            .primary_pid_for_live_pane_process(pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"pane_io":"write_failed","error":"{}"}}"#,
                json_escape(&pane_id),
                json_escape(&window_id),
                primary_pid,
                json_escape(&error)
            ),
        )?;
        self.fail_shell_transactions_for_pane_write_failure(&pane_id, &error)?;
        Ok(true)
    }

    /// Applies one pane input write completion delivered by an async pane driver.
    pub fn apply_pane_input_written_event(
        &mut self,
        pane_id: impl Into<String>,
        bytes: usize,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Ok(false);
        }
        let active_transactions = self
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.pane_id == pane_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
            .collect::<Vec<_>>();
        for (marker, transaction) in &active_transactions {
            let action_fragment = match &transaction.kind {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    format!(" action={action_id}")
                }
                RunningShellTransactionKind::ReadinessProbe
                | RunningShellTransactionKind::Bootstrap => String::new(),
            };
            self.append_agent_trace_turn_event(
                &pane_id,
                &transaction.turn_id,
                &format!(
                    "pane_input written bytes={} marker={} kind={}{}",
                    bytes,
                    marker,
                    runtime_running_shell_transaction_kind_name(&transaction.kind),
                    action_fragment
                ),
            )?;
        }
        Ok(!active_transactions.is_empty())
    }

    /// Clears strict marker protocol state for one settled shell transaction.
    pub(super) fn clear_shell_transaction_protocol_state(&mut self, marker: &str) {
        self.shell_transaction_require_start_markers.remove(marker);
        self.shell_transaction_started_markers.remove(marker);
    }

    /// Interrupts a pane after a protocol violation when the process is live.
    fn interrupt_shell_transaction_pane_if_live(&mut self, pane_id: &str) -> Result<()> {
        match self.interrupt_shell_transaction_pane(pane_id) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Fails one live shell transaction because its wrapper marker protocol
    /// reached an impossible state.
    fn fail_shell_transaction_protocol_violation(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        boundary_state: &'static str,
        message: impl Into<String>,
    ) -> Result<usize> {
        let message = message.into();
        self.runtime_metrics
            .record_shell_transaction_protocol_violation();
        self.running_shell_transactions.remove(marker);
        self.clear_shell_transaction_protocol_state(marker);
        self.interrupt_shell_transaction_pane_if_live(&transaction.pane_id)?;
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=shell_protocol_violation marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        match transaction.kind.clone() {
            RunningShellTransactionKind::AgentAction { action_id } => {
                let terminal_observation = shell_transaction_protocol_violation_observation(
                    marker,
                    &transaction,
                    boundary_state,
                    &message,
                );
                self.fail_running_shell_transaction_action(
                    &transaction,
                    marker,
                    RuntimeShellTransactionActionFailure {
                        action_id,
                        status: ActionStatus::Failed,
                        code: "shell_protocol_violation".to_string(),
                        message,
                        sent_to_pane: true,
                        terminal_observation,
                        trace_reason: "shell_protocol_violation".to_string(),
                    },
                )
            }
            RunningShellTransactionKind::ReadinessProbe => {
                self.pane_readiness_overrides
                    .clear_pending_probe(&transaction.pane_id);
                if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id)
                {
                    let terminal_observation = shell_transaction_protocol_violation_observation(
                        marker,
                        &transaction,
                        boundary_state,
                        &message,
                    );
                    self.fail_running_shell_transaction_action(
                        &transaction,
                        marker,
                        RuntimeShellTransactionActionFailure {
                            action_id,
                            status: ActionStatus::Failed,
                            code: "shell_protocol_violation".to_string(),
                            message,
                            sent_to_pane: false,
                            terminal_observation,
                            trace_reason: "shell_protocol_violation".to_string(),
                        },
                    )
                } else {
                    self.append_agent_error_text_to_terminal_buffer(
                        &transaction.pane_id,
                        &format!("agent: shell readiness probe protocol violation: {message}"),
                    )?;
                    Ok(1)
                }
            }
            RunningShellTransactionKind::Bootstrap => {
                self.pane_bootstrap_pending.remove(&transaction.pane_id);
                self.append_agent_error_text_to_terminal_buffer(
                    &transaction.pane_id,
                    &format!("agent: shell bootstrap protocol violation: {message}"),
                )?;
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"protocol_violation","marker":"{}","state":"degraded","error":"{}"}}"#,
                        json_escape(&transaction.pane_id),
                        json_escape(marker),
                        json_escape(&message)
                    ),
                )?;
                Ok(1)
            }
        }
    }

    /// Fails live shell transactions for a pane whose PTY input write failed.
    fn fail_shell_transactions_for_pane_write_failure(
        &mut self,
        pane_id: &str,
        error: &str,
    ) -> Result<usize> {
        let failed_transactions = self
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.pane_id == pane_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.clone()))
            .collect::<Vec<_>>();
        let mut failed_count = 0usize;
        for (marker, transaction) in failed_transactions {
            if self.running_shell_transactions.remove(&marker).is_none() {
                continue;
            }
            self.clear_shell_transaction_protocol_state(&marker);
            failed_count = failed_count.saturating_add(1);
            match transaction.kind.clone() {
                RunningShellTransactionKind::AgentAction { action_id } => {
                    self.fail_agent_action_for_pane_write_failure(
                        &marker,
                        transaction,
                        &action_id,
                        error,
                    )?;
                }
                RunningShellTransactionKind::ReadinessProbe => {
                    self.fail_readiness_probe_for_pane_write_failure(&marker, transaction, error)?;
                }
                RunningShellTransactionKind::Bootstrap => {
                    self.fail_bootstrap_for_pane_write_failure(&marker, transaction, error)?;
                }
            }
        }
        Ok(failed_count)
    }

    /// Fails one running agent action when its pane input cannot be written.
    fn fail_agent_action_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        action_id: &str,
        error: &str,
    ) -> Result<()> {
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=pane_input_write_failed marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        let terminal_observation = pane_write_failure_terminal_observation(
            marker,
            &transaction,
            "pane-input-write-failed",
            error,
        );
        let _ = self.fail_running_shell_transaction_action(
            &transaction,
            marker,
            RuntimeShellTransactionActionFailure {
                action_id: action_id.to_string(),
                status: ActionStatus::Failed,
                code: "pane_input_write_failed".to_string(),
                message: format!("pane input write failed while sending shell action: {error}"),
                sent_to_pane: false,
                terminal_observation,
                trace_reason: "pane_input_write_failed".to_string(),
            },
        )?;
        Ok(())
    }

    /// Fails a pending shell action when its readiness probe cannot be written.
    fn fail_readiness_probe_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        self.pane_readiness_overrides
            .clear_pending_probe(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_agent_trace_turn_event(
            &transaction.pane_id,
            &transaction.turn_id,
            &format!(
                "pane_readiness {} -> degraded reason=readiness_probe_pane_input_write_failed marker={}",
                runtime_pane_readiness_state_name(previous),
                marker
            ),
        )?;
        if let Some(action_id) = self.pending_shell_action_id_for_turn(&transaction.turn_id) {
            let terminal_observation = pane_write_failure_terminal_observation(
                marker,
                &transaction,
                "readiness-probe-pane-input-write-failed",
                error,
            );
            let _ = self.fail_running_shell_transaction_action(
                &transaction,
                marker,
                RuntimeShellTransactionActionFailure {
                    action_id,
                    status: ActionStatus::Failed,
                    code: "pane_input_write_failed".to_string(),
                    message: format!(
                        "pane input write failed while sending shell readiness probe: {error}"
                    ),
                    sent_to_pane: false,
                    terminal_observation,
                    trace_reason: "readiness_probe_pane_input_write_failed".to_string(),
                },
            )?;
        } else {
            self.append_agent_error_text_to_terminal_buffer(
                &transaction.pane_id,
                &format!("agent: shell readiness probe write failed: {error}"),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"write_failed","marker":"{}","error":"{}"}}"#,
                    json_escape(&transaction.pane_id),
                    json_escape(&transaction.turn_id),
                    json_escape(marker),
                    json_escape(error)
                ),
            )?;
        }
        Ok(())
    }

    /// Marks a bootstrap transaction degraded when its pane input cannot write.
    fn fail_bootstrap_for_pane_write_failure(
        &mut self,
        marker: &str,
        transaction: RunningShellTransactionRef,
        error: &str,
    ) -> Result<()> {
        self.pane_bootstrap_pending.remove(&transaction.pane_id);
        let previous = self.pane_readiness_state(&transaction.pane_id);
        self.set_pane_readiness(&transaction.pane_id, PaneReadinessState::Degraded);
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"write_failed","marker":"{}","previous_state":"{}","state":"degraded","error":"{}"}}"#,
                json_escape(&transaction.pane_id),
                json_escape(marker),
                runtime_pane_readiness_state_name(previous),
                json_escape(error)
            ),
        )?;
        Ok(())
    }

    /// Applies one PTY resize completion delivered by an async pane driver.
    pub fn apply_pane_resize_completion_event(
        &mut self,
        pane_id: impl Into<String>,
        size: Size,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(descriptor) = self.find_pane_descriptor(&pane_id) else {
            return Ok(false);
        };
        if let Some(screen) = self.pane_screens.get_mut(descriptor.pane_id.as_str()) {
            screen.resize(size);
        }
        if let Some(screen) = self
            .pane_transaction_osc_screens
            .get_mut(descriptor.pane_id.as_str())
        {
            screen.resize(size);
        }
        let primary_pid = self
            .pane_processes
            .primary_pid(descriptor.pane_id.as_str())
            .unwrap_or(0);
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"pty_resize":"applied","columns":{},"rows":{}}}"#,
                json_escape(descriptor.pane_id.as_str()),
                json_escape(descriptor.window_id.as_str()),
                primary_pid,
                size.columns,
                size.rows
            ),
        )?;
        Ok(true)
    }

    /// Runs the apply exited pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_exited_pane_process(
        &mut self,
        process: ExitedPaneProcess,
        remove_recorded_process: bool,
    ) -> Result<PaneExitUpdate> {
        let descriptor = self.find_pane_descriptor(&process.pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "exited pane process has no matching pane",
            )
        })?;
        let previous_window_count = self.session.windows().len();

        let _ = self.stop_active_pane_pipe(process.pane_id.as_str());
        self.pane_current_working_directories
            .remove(process.pane_id.as_str());
        self.fail_agent_turns_for_pane_shutdown(
            std::slice::from_ref(&process.pane_id),
            "pane primary process exited",
        )?;
        self.pane_exit_records.insert(
            process.pane_id.clone(),
            PaneExitRecord {
                exit_status: process.status,
            },
        );
        self.close_exited_pane(&descriptor)?;
        if !self.session.windows().is_empty() {
            self.sync_tracked_pty_sizes()?;
        }
        if remove_recorded_process {
            self.pane_processes.remove_exited(&process.pane_id)?;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);

        let closed_window = self.session.windows().len() < previous_window_count;
        let update = PaneExitUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid: process.primary_pid,
            exit_status: process.status,
            closed_window,
            session_empty: self.session.windows().is_empty(),
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_exit_event(&update)?;
        if closed_window {
            self.append_lifecycle_event(
                EventKind::WindowChanged,
                format!(
                    r#"{{"window_id":"{}","state":"closed","session_empty":{}}}"#,
                    json_escape(&update.window_id),
                    update.session_empty
                ),
            )?;
        }
        self.persist_or_defer_registry_update_plan(update.registry_update.clone())?;
        Ok(update)
    }

    /// Runs the poll pane outputs operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn poll_pane_outputs(
        &mut self,
        max_bytes_per_pane: usize,
    ) -> Result<Vec<PaneOutputUpdate>> {
        self.require_live()?;
        let outputs = self
            .pane_processes
            .read_available_output(max_bytes_per_pane)?;
        let mut updates = Vec::new();
        let mut terminal_title_panes = BTreeSet::new();

        for output in outputs {
            updates.push(self.apply_pane_process_output(output, &mut terminal_title_panes)?);
        }

        if self.should_sync_pane_titles_from_foreground_processes(!updates.is_empty()) {
            self.sync_pane_titles_from_foreground_processes(&terminal_title_panes)?;
        }

        Ok(updates)
    }

    /// Applies one pane-output event delivered by an async pane driver.
    ///
    /// This is the event-driven equivalent of one item returned by
    /// `PaneProcessManager::read_available_output`. It preserves the same
    /// filtering, OSC observation, screen feeding, shell transaction tracking,
    /// pane-pipe forwarding, title syncing, and event-log behavior used by the
    /// synchronous polling path.
    pub fn apply_pane_output_bytes(
        &mut self,
        pane_id: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Option<PaneOutputUpdate>> {
        self.require_live()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let pane_id = pane_id.into();
        let primary_pid = self
            .primary_pid_for_live_pane_process(&pane_id)
            .unwrap_or(0);
        let mut terminal_title_panes = BTreeSet::new();
        let update = self.apply_pane_process_output(
            PaneProcessOutput {
                pane_id,
                primary_pid,
                bytes,
            },
            &mut terminal_title_panes,
        )?;
        if self.should_sync_pane_titles_from_foreground_processes(true) {
            self.sync_pane_titles_from_foreground_processes(&terminal_title_panes)?;
        }
        Ok(Some(update))
    }

    /// Runs the apply pane process output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn apply_pane_process_output(
        &mut self,
        output: PaneProcessOutput,
        terminal_title_panes: &mut BTreeSet<String>,
    ) -> Result<PaneOutputUpdate> {
        let descriptor = self.find_pane_descriptor(&output.pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane output has no matching pane",
            )
        })?;
        let descriptor_size = descriptor.size;
        let descriptor_window_id = descriptor.window_id.to_string();
        let background = self
            .session
            .active_window()
            .is_none_or(|window| window.active_pane().id.as_str() != descriptor.pane_id.as_str());
        let transaction_bytes =
            self.visible_pane_output_bytes(output.pane_id.as_str(), &output.bytes);
        let render_bytes =
            self.renderable_pane_output_bytes(output.pane_id.as_str(), &transaction_bytes);
        let (osc_events, transaction_alternate_active) = self.terminal_osc_events_for_pane_bytes(
            output.pane_id.as_str(),
            descriptor_size,
            &transaction_bytes,
        )?;
        let (title, activity_events, bell_events, render_alternate_active) = {
            let screen = self.pane_screens.entry(output.pane_id.clone()).or_insert(
                TerminalScreen::new_with_history_config(
                    descriptor_size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?,
            );
            let previous_activity_events = screen.activity_events();
            let previous_bell_events = screen.bell_events();
            screen.feed(&render_bytes);
            let _ = screen.drain_osc_events();
            (
                screen.title().map(ToOwned::to_owned),
                screen
                    .activity_events()
                    .saturating_sub(previous_activity_events),
                screen.bell_events().saturating_sub(previous_bell_events),
                screen.alternate_screen_active(),
            )
        };
        let alternate_active = transaction_alternate_active || render_alternate_active;
        let terminal_title = osc_events.iter().rev().find_map(|event| match event {
            TerminalOscEvent::TitleChanged { title } => Some(title.clone()),
            _ => None,
        });
        if terminal_title.is_some() {
            terminal_title_panes.insert(output.pane_id.clone());
        }
        self.apply_terminal_osc_events(&osc_events)?;
        if alternate_active {
            self.pane_readiness_overrides.revoke(
                output.pane_id.as_str(),
                ReadinessOverrideRevocation::AlternateScreenEntry,
            );
            self.set_pane_readiness(
                output.pane_id.as_str(),
                PaneReadinessState::InteractiveBlocked,
            );
        }
        self.record_running_shell_transaction_output(output.pane_id.as_str(), &transaction_bytes);
        self.observe_agent_shell_transaction_events(output.pane_id.as_str(), &osc_events)?;
        self.write_active_pane_pipe(output.pane_id.as_str(), &render_bytes)?;
        let title_changed = if let Some(title) = terminal_title.or(title) {
            self.session
                .set_pane_title_from_terminal(output.pane_id.as_str(), title)?
        } else {
            false
        };

        let update = PaneOutputUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor_window_id,
            pane_id: output.pane_id,
            primary_pid: output.primary_pid,
            bytes_read: output.bytes.len(),
            activity_events,
            bell_events,
            background,
        };
        self.append_pane_output_event(&update)?;
        if title_changed {
            self.append_pane_title_event(&update)?;
        }
        Ok(update)
    }

    /// Returns pane bytes that should be retained for active Mezzanine-owned
    /// shell transactions after filtering wrapper echo that is irrelevant to the
    /// model and the runtime state machine.
    ///
    /// Interactive shells echo the wrapper lines that Mezzanine writes around
    /// agent actions, readiness probes, and bootstrap probes. Those lines are
    /// implementation traffic, not user commands, so normal transaction
    /// observation hides them while preserving command output and the OSC
    /// transaction markers that drive the runtime state machine. Trace logging
    /// disables this filter for diagnosis.
    pub(super) fn visible_pane_output_bytes(&mut self, pane_id: &str, bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }
        let active_transaction = self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id);
        let filter_commands = self.mez_wrapper_filter_commands_for_pane(pane_id);
        if self.agent_trace_enabled(pane_id)
            || (filter_commands.is_empty()
                && !mez_wrapper_filter_bytes_may_contain_boilerplate(bytes))
        {
            let mut visible = self
                .pane_mez_wrapper_filter_pending
                .remove(pane_id)
                .unwrap_or_default();
            visible.extend_from_slice(bytes);
            if !active_transaction {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
            return visible;
        }

        let mut pending = self
            .pane_mez_wrapper_filter_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(bytes);
        let mut visible = Vec::with_capacity(pending.len());
        let mut filtered_wrapper_echo = false;
        let mut line_start = 0usize;
        while let Some(relative_terminator) = pending[line_start..]
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r')
        {
            let terminator = line_start + relative_terminator;
            let line_end = if pending[terminator] == b'\r'
                && pending
                    .get(terminator + 1)
                    .is_some_and(|byte| *byte == b'\n')
            {
                terminator + 2
            } else {
                terminator + 1
            };
            let line = &pending[line_start..line_end];
            let filtered_line = mez_wrapper_echo_line_visible_bytes(line, &filter_commands);
            if filtered_line.len() != line.len() {
                filtered_wrapper_echo = true;
            }
            visible.extend_from_slice(&filtered_line);
            line_start = line_end;
        }

        if line_start < pending.len() {
            let tail = &pending[line_start..];
            if tail.contains(&0x1b) {
                let filtered_tail = mez_wrapper_echo_line_visible_bytes(tail, &filter_commands);
                if filtered_tail.len() != tail.len() {
                    filtered_wrapper_echo = true;
                }
                visible.extend_from_slice(&filtered_tail);
            } else if mez_wrapper_echo_line_is_hidden(tail, &filter_commands) {
                filtered_wrapper_echo = true;
            } else if !mez_wrapper_echo_line_is_possible_prefix(tail, &filter_commands) {
                visible.extend_from_slice(tail);
            } else {
                filtered_wrapper_echo = true;
                self.pane_mez_wrapper_filter_pending
                    .insert(pane_id.to_string(), tail.to_vec());
            }
        }
        if !active_transaction {
            if filtered_wrapper_echo {
                self.pane_mez_wrapper_filter_recent_polls.insert(
                    pane_id.to_string(),
                    RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
                );
            } else {
                self.tick_mez_wrapper_filter_retention(pane_id);
            }
        }
        visible
    }

    /// Runs the pane output render mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_output_render_mode(&self, pane_id: &str) -> PaneOutputRenderMode {
        if self.agent_trace_enabled(pane_id) {
            return PaneOutputRenderMode::Trace;
        }
        let shell_view_enabled = self.agent_shell_view_enabled(pane_id);
        let mut has_agent_action = false;
        for transaction in self
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
        {
            match &transaction.kind {
                RunningShellTransactionKind::AgentAction { .. } => {
                    has_agent_action = true;
                }
                RunningShellTransactionKind::ReadinessProbe
                | RunningShellTransactionKind::Bootstrap => {
                    return PaneOutputRenderMode::HiddenLiveAgentShell;
                }
            }
        }
        if has_agent_action {
            if shell_view_enabled {
                PaneOutputRenderMode::VerboseAgentAction
            } else {
                PaneOutputRenderMode::HiddenLiveAgentShell
            }
        } else if !shell_view_enabled
            && (self.pane_has_running_agent_turn(pane_id)
                || self.pane_agent_subshell_active(pane_id))
        {
            PaneOutputRenderMode::HiddenLiveAgentShell
        } else if !shell_view_enabled
            && self
                .pane_hidden_shell_render_recent_polls
                .contains_key(pane_id)
        {
            PaneOutputRenderMode::HiddenRetainedAgentShell
        } else {
            PaneOutputRenderMode::Normal
        }
    }

    /// Runs the renderable pane output bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn renderable_pane_output_bytes(
        &mut self,
        pane_id: &str,
        transaction_bytes: &[u8],
    ) -> Vec<u8> {
        match self.pane_output_render_mode(pane_id) {
            PaneOutputRenderMode::Normal
            | PaneOutputRenderMode::VerboseAgentAction
            | PaneOutputRenderMode::Trace => transaction_bytes.to_vec(),
            PaneOutputRenderMode::HiddenLiveAgentShell => {
                if !transaction_bytes.is_empty() {
                    self.remember_hidden_shell_render_suppression(pane_id);
                }
                Vec::new()
            }
            PaneOutputRenderMode::HiddenRetainedAgentShell => Vec::new(),
        }
    }

    /// Reports whether the pane has a runtime agent turn currently occupying
    /// the pane's agent shell session.
    fn pane_has_running_agent_turn(&self, pane_id: &str) -> bool {
        self.agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
    }

    /// Reports whether a pane currently owns a child shell for agent mode.
    ///
    /// The child shell's prompt and setup repaint are implementation traffic
    /// unless shell-view diagnostics are enabled.
    fn pane_agent_subshell_active(&self, pane_id: &str) -> bool {
        self.agent_subshell_panes.contains(pane_id)
    }

    /// Retains short-lived shell-output suppression after a hidden agent shell
    /// transaction so delayed prompt repaint bytes do not leak into the pane.
    pub(super) fn remember_hidden_shell_render_suppression(&mut self, pane_id: &str) {
        self.pane_hidden_shell_render_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS,
        );
    }

    /// Clears retained shell-output filters for explicit foreground input.
    ///
    /// Hidden-shell and wrapper-echo retention suppress delayed implementation
    /// prompt repaint bytes after agent-owned shell work. Once foreground
    /// control returns to the pane, following PTY output belongs to the user's
    /// interaction and must not be swallowed or reduced to cursor-control
    /// remnants by the previous agent turn's cleanup window.
    pub(super) fn clear_shell_output_filters_for_foreground_input(&mut self, pane_id: &str) {
        self.pane_hidden_shell_render_recent_polls.remove(pane_id);
        self.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
    }

    /// Ages out retained shell-output suppression for panes whose agent turn and
    /// Mezzanine-owned shell transaction have both settled.
    pub(super) fn tick_hidden_shell_render_retention(&mut self) -> usize {
        let mut aged = 0usize;
        let retained = self
            .pane_hidden_shell_render_recent_polls
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for pane_id in retained {
            if self.pane_has_running_agent_turn(&pane_id)
                || self
                    .running_shell_transactions
                    .values()
                    .any(|transaction| transaction.pane_id == pane_id)
            {
                continue;
            }
            let Some(remaining) = self.pane_hidden_shell_render_recent_polls.get_mut(&pane_id)
            else {
                continue;
            };
            *remaining = remaining.saturating_sub(1);
            aged = aged.saturating_add(1);
            if *remaining == 0 {
                self.pane_hidden_shell_render_recent_polls.remove(&pane_id);
            }
        }
        aged
    }

    /// Applies runtime idle-cleanup timer work that does not require polling PTY
    /// or process state.
    ///
    /// Migrated cleanup targets include hidden-shell render suppression
    /// retention, stranded agent dispatch recovery, and unreachable running
    /// turn failure. These operations
    /// are driven by actor timer events in the daemon path so idle sessions do
    /// not need to scan them through the compatibility tick.
    pub fn apply_idle_cleanup_timer_event(&mut self) -> Result<usize> {
        self.apply_idle_cleanup_timer_event_with_actor_progress(&BTreeSet::new())
    }

    /// Applies runtime idle-cleanup timer work while honoring actor-owned
    /// progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn apply_idle_cleanup_timer_event_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        match self.lifecycle_state {
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed => Ok(0),
            RuntimeLifecycleState::Running
            | RuntimeLifecycleState::Detached
            | RuntimeLifecycleState::Stopping => {
                let hidden_shell_retention_aged = self.tick_hidden_shell_render_retention();
                let reconciled = self.reconcile_agent_runtime_progress_paths_with_actor_progress(
                    actor_progress_turn_ids,
                )?;
                Ok(hidden_shell_retention_aged.saturating_add(reconciled))
            }
        }
    }

    /// Reconciles running agent turns after actor-owned state transitions.
    ///
    /// The runtime specification requires every running turn to retain an
    /// observable progress path. Calling this after event application prevents
    /// stranded turns from depending on a later idle timer before they are
    /// requeued or failed.
    pub fn reconcile_agent_runtime_progress_paths(&mut self) -> Result<usize> {
        self.reconcile_agent_runtime_progress_paths_with_actor_progress(&BTreeSet::new())
    }

    /// Reconciles running agent turns while honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Turns waiting on progress owned by the
    ///   async actor, such as provider retry timers that are not represented in
    ///   service-owned queues.
    pub fn reconcile_agent_runtime_progress_paths_with_actor_progress(
        &mut self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> Result<usize> {
        if matches!(
            self.lifecycle_state,
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
        ) {
            return Ok(0);
        }
        let stranded_shell_recoveries = self.recover_stranded_agent_shell_dispatches()?;
        let unreachable_turn_failures =
            self.fail_unreachable_running_agent_turns_with_actor_progress(actor_progress_turn_ids)?;
        Ok(stranded_shell_recoveries.saturating_add(unreachable_turn_failures))
    }

    /// Reports whether actor-owned idle cleanup should remain scheduled.
    ///
    /// Hidden agent-shell render suppression retention must age out after the
    /// shell transaction and running turn settle so delayed prompt bytes do not
    /// leak into normal pane rendering. Stranded shell-dispatch recovery must
    /// also retry while a pending shell command is blocked behind stale
    /// readiness state.
    pub fn idle_cleanup_timer_needed(&self) -> bool {
        self.idle_cleanup_timer_needed_with_actor_progress(&BTreeSet::new())
    }

    /// Reports whether actor-owned idle cleanup should remain scheduled while
    /// honoring actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn idle_cleanup_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        self.hidden_shell_render_retention_timer_needed()
            || self.stranded_agent_shell_dispatch_recovery_timer_needed()
            || self.unreachable_running_agent_turn_timer_needed_with_actor_progress(
                actor_progress_turn_ids,
            )
    }

    /// Reports whether hidden shell-render suppression still needs to age out.
    pub fn hidden_shell_render_retention_timer_needed(&self) -> bool {
        !self.pane_hidden_shell_render_recent_polls.is_empty()
    }

    /// Reports whether any pending agent shell dispatch may need recovery.
    pub fn stranded_agent_shell_dispatch_recovery_timer_needed(&self) -> bool {
        !self
            .stranded_agent_shell_dispatch_recovery_candidates()
            .is_empty()
    }

    /// Reports whether any running turn has no remaining runtime progress path.
    pub fn unreachable_running_agent_turn_timer_needed(&self) -> bool {
        self.unreachable_running_agent_turn_timer_needed_with_actor_progress(&BTreeSet::new())
    }

    /// Reports whether any running turn has no remaining runtime progress path
    /// after accounting for async actor-owned progress.
    ///
    /// # Parameters
    /// - `actor_progress_turn_ids`: Running turns with progress represented by
    ///   async actor state rather than service-owned queues.
    pub fn unreachable_running_agent_turn_timer_needed_with_actor_progress(
        &self,
        actor_progress_turn_ids: &BTreeSet<String>,
    ) -> bool {
        !self
            .unreachable_running_agent_turn_candidates(actor_progress_turn_ids)
            .is_empty()
    }

    /// Runs the terminal osc events for pane bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn terminal_osc_events_for_pane_bytes(
        &mut self,
        pane_id: &str,
        size: Size,
        bytes: &[u8],
    ) -> Result<(Vec<TerminalOscEvent>, bool)> {
        if bytes.is_empty() {
            return Ok((Vec::new(), false));
        }
        if matches!(
            self.pane_output_render_mode(pane_id),
            PaneOutputRenderMode::HiddenLiveAgentShell
                | PaneOutputRenderMode::HiddenRetainedAgentShell
        ) {
            return Ok((
                self.hidden_agent_shell_osc_events_for_pane_bytes(pane_id, bytes),
                false,
            ));
        }
        let screen = if let Some(screen) = self.pane_transaction_osc_screens.get_mut(pane_id) {
            screen.resize(size);
            screen
        } else {
            self.pane_transaction_osc_screens.insert(
                pane_id.to_string(),
                TerminalScreen::new_with_history_config(
                    size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?,
            );
            self.pane_transaction_osc_screens
                .get_mut(pane_id)
                .ok_or_else(|| {
                    MezError::invalid_state("transaction OSC parser was not retained for pane")
                })?
        };
        screen.feed(bytes);
        Ok((screen.drain_osc_events(), screen.alternate_screen_active()))
    }

    /// Scans hidden agent-shell bytes for Mezzanine-owned OSC transaction
    /// markers without feeding arbitrary command output into a terminal parser.
    ///
    /// Hidden agent-shell output is command data for the model. Treating long
    /// file bodies or embedded escape sequences as terminal traffic can
    /// monopolize the runtime actor and mutate parser state. This scanner keeps
    /// only a bounded fragment that may contain a split `ESC ] 133` marker and
    /// ignores all other hidden bytes.
    fn hidden_agent_shell_osc_events_for_pane_bytes(
        &mut self,
        pane_id: &str,
        bytes: &[u8],
    ) -> Vec<TerminalOscEvent> {
        let mut pending = self
            .pane_transaction_osc_pending
            .remove(pane_id)
            .unwrap_or_default();
        pending.extend_from_slice(bytes);
        let (events, retained) = scan_mezzanine_osc_transaction_events(&pending);
        if retained.is_empty() {
            self.pane_transaction_osc_pending.remove(pane_id);
        } else {
            self.pane_transaction_osc_pending
                .insert(pane_id.to_string(), retained);
        }
        events
    }

    /// Runs the remember mez wrapper filter command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn remember_mez_wrapper_filter_command(&mut self, pane_id: &str, command: &str) {
        let retained = self
            .pane_mez_wrapper_filter_recent_commands
            .entry(pane_id.to_string())
            .or_default();
        for line in command
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if !retained.iter().any(|existing| existing == line) {
                retained.push(line.to_string());
            }
        }
        let extra = retained
            .len()
            .saturating_sub(RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT);
        if extra > 0 {
            retained.drain(0..extra);
        }
        self.pane_mez_wrapper_filter_recent_polls.insert(
            pane_id.to_string(),
            RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS,
        );
    }

    /// Runs the mez wrapper filter commands for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn mez_wrapper_filter_commands_for_pane(&self, pane_id: &str) -> Vec<String> {
        let mut commands = self
            .running_shell_transactions
            .values()
            .filter(|transaction| transaction.pane_id == pane_id)
            .flat_map(|transaction| {
                transaction
                    .command
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if let Some(retained) = self.pane_mez_wrapper_filter_recent_commands.get(pane_id) {
            for command in retained {
                if !commands.iter().any(|existing| existing == command) {
                    commands.push(command.clone());
                }
            }
        }
        commands
    }

    /// Runs the tick mez wrapper filter retention operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn tick_mez_wrapper_filter_retention(&mut self, pane_id: &str) {
        let Some(remaining) = self.pane_mez_wrapper_filter_recent_polls.get_mut(pane_id) else {
            return;
        };
        *remaining = remaining.saturating_sub(1);
        if *remaining == 0 {
            self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
            self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
            self.pane_mez_wrapper_filter_pending.remove(pane_id);
        }
    }

    /// Runs the sync pane titles from foreground processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn sync_pane_titles_from_foreground_processes(
        &mut self,
        skipped_panes: &BTreeSet<String>,
    ) -> Result<usize> {
        let mut changed = 0usize;
        for pane_id in self.pane_processes.tracked_pane_ids() {
            if skipped_panes.contains(&pane_id) {
                continue;
            }
            let Some(title) = self.foreground_process_pane_title(&pane_id) else {
                continue;
            };
            if !self
                .session
                .set_pane_title_from_terminal(pane_id.as_str(), title)?
            {
                continue;
            }
            let Some(update) = self.pane_title_only_update(&pane_id) else {
                continue;
            };
            self.append_pane_title_event(&update)?;
            changed = changed.saturating_add(1);
        }
        Ok(changed)
    }

    /// Runs the should sync pane titles from foreground processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn should_sync_pane_titles_from_foreground_processes(&mut self, observed_output: bool) -> bool {
        if observed_output {
            self.foreground_title_idle_sync_polls = 0;
            return true;
        }
        let should_sync = self.foreground_title_idle_sync_polls == 0;
        self.foreground_title_idle_sync_polls =
            self.foreground_title_idle_sync_polls.saturating_add(1)
                % RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL;
        should_sync
    }

    /// Runs the foreground process pane title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn foreground_process_pane_title(&self, pane_id: &str) -> Option<String> {
        let foreground_name = self.pane_processes.foreground_process_name(pane_id)?;
        let foreground_group = self.pane_processes.foreground_process_group_id(pane_id)?;
        let primary_pid = self.pane_processes.primary_pid(pane_id)?;
        self.title_from_foreground_process_metadata(
            pane_id,
            foreground_name,
            foreground_group,
            primary_pid,
        )
    }

    /// Applies foreground process metadata delivered by an async pane worker.
    pub fn apply_pane_foreground_process_event(
        &mut self,
        pane_id: impl Into<String>,
        process_name: impl Into<String>,
        process_group_id: u32,
        current_working_directory: Option<String>,
    ) -> Result<bool> {
        self.require_live()?;
        let pane_id = pane_id.into();
        let Some(primary_pid) = self.primary_pid_for_live_pane_process(&pane_id) else {
            return Ok(false);
        };
        if let Some(current_working_directory) = current_working_directory
            && !current_working_directory.trim().is_empty()
        {
            self.pane_current_working_directories
                .insert(pane_id.clone(), PathBuf::from(current_working_directory));
        }
        let Some(title) = self.title_from_foreground_process_metadata(
            &pane_id,
            process_name.into(),
            process_group_id,
            primary_pid,
        ) else {
            return Ok(false);
        };
        if !self
            .session
            .set_pane_title_from_terminal(pane_id.as_str(), title)?
        {
            return Ok(false);
        }
        let Some(update) = self.pane_title_only_update(&pane_id) else {
            return Ok(false);
        };
        self.append_pane_title_event(&update)?;
        Ok(true)
    }

    /// Returns the best known current working directory for a live pane.
    ///
    /// Async pane workers publish foreground metadata into
    /// `pane_current_working_directories`; prefer that actor-owned snapshot so
    /// command planning does not synchronously probe host process metadata when
    /// an async observation is already available.
    pub(super) fn pane_current_working_directory(&self, pane_id: &str) -> Option<PathBuf> {
        self.pane_current_working_directories
            .get(pane_id)
            .cloned()
            .or_else(|| self.pane_processes.current_working_directory(pane_id))
    }

    /// Runs the title from foreground process metadata operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn title_from_foreground_process_metadata(
        &self,
        pane_id: &str,
        foreground_name: String,
        foreground_group: u32,
        primary_pid: u32,
    ) -> Option<String> {
        if foreground_group == primary_pid
            && Some(foreground_name.as_str()) == self.session.shell.path().file_name()?.to_str()
        {
            return self
                .pane_screens
                .get(pane_id)
                .and_then(TerminalScreen::title)
                .filter(|title| !title.trim().is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| Some("shell".to_string()));
        }
        Some(foreground_name)
    }

    /// Runs the pane title only update operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_title_only_update(&self, pane_id: &str) -> Option<PaneOutputUpdate> {
        let descriptor = self.find_pane_descriptor(pane_id)?;
        Some(PaneOutputUpdate {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: pane_id.to_string(),
            primary_pid: self.primary_pid_for_live_pane_process(pane_id)?,
            bytes_read: 0,
            activity_events: 0,
            bell_events: 0,
            background: !self.session.active_window().is_some_and(|window| {
                window.active_pane().id.as_str() == descriptor.pane_id.as_str()
            }),
        })
    }

    /// Runs the record running shell transaction output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_readiness_probe_to_pane(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        if self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == turn.pane_id)
        {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "readiness_probe dispatch skipped reason=shell_transaction_running",
            )?;
            return Ok(());
        }
        if self.running_shell_transactions.values().any(|transaction| {
            transaction.turn_id == turn.turn_id
                && transaction.kind == RunningShellTransactionKind::ReadinessProbe
        }) {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "readiness_probe dispatch skipped reason=already_running",
            )?;
            return Ok(());
        }
        let previous_readiness = self.pane_readiness_state(&turn.pane_id);
        let marker = runtime_marker_for_action(turn, "readiness-probe")?;
        let marker_id = marker.as_str().to_string();
        let classification = self.shell_classification_for_pane(&turn.pane_id);
        let probe_command = readiness_probe_command_for_classification(classification);
        let transaction = ShellTransaction::new(
            marker,
            &turn.turn_id,
            &turn.agent_id,
            &turn.pane_id,
            self.session.shell.path(),
            probe_command,
        )?;
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(&turn.pane_id, probe_command);
        self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes())?;
        self.pane_readiness_overrides
            .record_pending_probe(&turn.pane_id)?;
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Probing);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_readiness {} -> probing reason=readiness_probe_sent marker={}",
                runtime_pane_readiness_state_name(previous_readiness),
                marker_id
            ),
        )?;
        self.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::ReadinessProbe,
                pane_id: turn.pane_id.clone(),
                command: probe_command.to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(RUNTIME_READINESS_PROBE_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.shell_transaction_require_start_markers
            .insert(marker_id.clone());
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"probing","readiness_probe":"sent","marker":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(())
    }

    /// Runs the observe readiness probe transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_readiness_probe_transaction_end(
        &mut self,
        marker: &str,
        turn_id: &str,
        agent_id: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.agent_id != agent_id || turn.pane_id != pane_id {
            return Err(MezError::invalid_state(
                "readiness probe marker identity does not match agent turn",
            ));
        }
        self.pane_readiness_overrides.clear_pending_probe(pane_id);
        if exit_code == 0 {
            let previous_readiness = self.pane_readiness_state(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
            self.append_agent_trace_turn_event(
                pane_id,
                turn_id,
                &format!(
                    "pane_readiness {} -> ready reason=readiness_probe_completed marker={}",
                    runtime_pane_readiness_state_name(previous_readiness),
                    marker
                ),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"ready","readiness_probe":"completed","marker":"{}","exit_code":0}}"#,
                    json_escape(pane_id),
                    json_escape(turn_id),
                    json_escape(marker)
                ),
            )?;
            let should_dispatch_stored_shell =
                self.agent_turn_executions
                    .get(turn_id)
                    .is_some_and(|execution| {
                        self.execution_has_pending_shell_dispatch(turn_id, execution)
                    });
            if should_dispatch_stored_shell {
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "pending_shell_dispatch available reason=readiness_probe_completed",
                )?;
                let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
            } else if self
                .agent_turn_ledger
                .turns()
                .iter()
                .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
                && self
                    .agent_turn_executions
                    .get(turn_id)
                    .is_some_and(runtime_execution_ready_for_provider_continuation)
            {
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    pane_id,
                    turn_id,
                    "provider_task queued reason=readiness_probe_completed",
                )?;
            }
        } else {
            self.pane_readiness_overrides
                .revoke(pane_id, ReadinessOverrideRevocation::ReadinessProbeFailed);
            let previous_readiness = self.pane_readiness_state(pane_id);
            self.set_pane_readiness(pane_id, PaneReadinessState::Degraded);
            self.append_agent_trace_turn_event(
                pane_id,
                turn_id,
                &format!(
                    "pane_readiness {} -> degraded reason=readiness_probe_failed marker={} exit_code={}",
                    runtime_pane_readiness_state_name(previous_readiness),
                    marker,
                    exit_code
                ),
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"degraded","readiness_probe":"failed","marker":"{}","exit_code":{}}}"#,
                    json_escape(pane_id),
                    json_escape(turn_id),
                    json_escape(marker),
                    exit_code
                ),
            )?;
        }
        Ok(1)
    }

    /// Runs the dispatch bootstrap to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_bootstrap_to_pane(&mut self, pane_id: &str) -> Result<()> {
        if self
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id)
        {
            return Ok(());
        }
        let agent_id = format!("agent-{pane_id}");
        let turn_id = format!("bootstrap-{pane_id}-{}", current_unix_seconds());
        let marker = runtime_random_marker_token(&format!("bootstrap\0{pane_id}\0{turn_id}"))?;
        let marker_id = marker.as_str().to_string();
        let classification = self.shell_classification_for_pane(pane_id);
        let bootstrap_script = bootstrap_script_for_classification(classification);
        let transaction = ShellTransaction::new(
            marker,
            &turn_id,
            &agent_id,
            pane_id,
            self.session.shell.path(),
            bootstrap_script.clone(),
        )?;
        let transaction_input = transaction.render_for_classification_input(classification);
        let mut wrapper = transaction_input.wrapper;
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        self.remember_mez_wrapper_filter_command(pane_id, &bootstrap_script);
        self.write_runtime_pane_input(pane_id, wrapper.as_bytes())?;
        self.set_pane_readiness(pane_id, PaneReadinessState::Busy);
        self.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn_id.clone(),
                kind: RunningShellTransactionKind::Bootstrap,
                pane_id: pane_id.to_string(),
                command: bootstrap_script,
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(DEFAULT_BOOTSTRAP_TIMEOUT_MS),
                pending_input_payload: (!transaction_input.payload.is_empty())
                    .then(|| transaction_input.payload.into_bytes()),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.shell_transaction_require_start_markers
            .insert(marker_id.clone());
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","bootstrap":"sent","marker":"{}"}}"#,
                json_escape(pane_id),
                json_escape(&marker_id)
            ),
        )?;
        Ok(())
    }

    /// Runs the observe bootstrap transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_bootstrap_transaction_end(
        &mut self,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
        observed_output_preview: &str,
        observed_output_truncated: bool,
    ) -> Result<usize> {
        self.pane_bootstrap_pending.remove(pane_id);
        let mut bootstrap_parsed = false;
        if exit_code == 0 {
            let all_output = if observed_output_preview.trim().is_empty() {
                let screen = self.pane_screens.get(pane_id).ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "pane terminal screen not found",
                    )
                })?;
                screen.normal_content_lines().join("\n")
            } else {
                observed_output_preview.to_string()
            };

            let (signature, inventory, instruction_files) =
                parse_bootstrap_env_output(&all_output, self.session.shell.path());

            if let Some(sig) = signature.clone() {
                bootstrap_parsed = true;
                self.pane_environment_signatures
                    .insert(pane_id.to_string(), sig.clone());
                if let Some(inv) = inventory.clone() {
                    self.tool_discovery_cache.record(sig, inv);
                }
                if !instruction_files.is_empty() {
                    self.pane_instruction_files
                        .insert(pane_id.to_string(), instruction_files);
                }
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"completed","marker":"{}","exit_code":0,"output_truncated":{}}}"#,
                        json_escape(pane_id),
                        json_escape(marker),
                        observed_output_truncated
                    ),
                )?;
            } else {
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","bootstrap":"unparsed","marker":"{}","exit_code":0,"output_truncated":{},"message":"bootstrap completed but no environment signature was parsed; continuing with degraded context"}}"#,
                        json_escape(pane_id),
                        json_escape(marker),
                        observed_output_truncated
                    ),
                )?;
            }
        } else {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","bootstrap":"failed","marker":"{}","exit_code":{}}}"#,
                    json_escape(pane_id),
                    json_escape(marker),
                    exit_code
                ),
            )?;
        }
        if bootstrap_parsed {
            self.set_pane_readiness(pane_id, PaneReadinessState::Ready);
        } else if self.pane_readiness_state(pane_id) == PaneReadinessState::Busy {
            self.set_pane_readiness(pane_id, PaneReadinessState::PromptCandidate);
        }
        let pending_shell_turns = self
            .agent_turn_executions
            .iter()
            .filter(|(turn_id, execution)| {
                self.execution_has_pending_shell_dispatch(turn_id, execution)
                    && self.agent_turn_ledger.turns().iter().any(|turn| {
                        turn.turn_id == **turn_id
                            && turn.pane_id == pane_id
                            && turn.state == AgentTurnState::Running
                    })
            })
            .map(|(turn_id, _)| turn_id.clone())
            .collect::<Vec<_>>();
        for turn_id in pending_shell_turns {
            let _ = self.dispatch_stored_running_shell_actions(&turn_id)?;
        }
        let _ = self.recover_stranded_agent_shell_dispatches()?;
        Ok(1)
    }

    /// Dispatches hidden bootstrap wrappers for pending panes that have reached
    /// prompt-like readiness.
    pub(crate) fn maybe_bootstrap_ready_panes(&mut self) -> Result<usize> {
        let ready_panes: Vec<String> = self
            .pane_readiness_states
            .iter()
            .filter(|(k, v)| {
                self.pane_bootstrap_pending.contains(k.as_str())
                    && !self
                        .running_shell_transactions
                        .values()
                        .any(|transaction| transaction.pane_id == k.as_str())
                    && matches!(
                        v,
                        PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
                    )
            })
            .map(|(k, _)| k.clone())
            .collect();
        let dispatches = ready_panes.len();
        for pane_id in ready_panes {
            self.dispatch_bootstrap_to_pane(&pane_id)?;
        }
        Ok(dispatches)
    }

    /// Runs the observe focused shell hook transaction end operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn observe_focused_shell_hook_transaction_end(
        &mut self,
        output_pane_id: &str,
        marker: &str,
        pane_id: &str,
        exit_code: i32,
    ) -> Result<usize> {
        let Some(pending) = self.focused_shell_hook_transactions.remove(marker) else {
            return Ok(0);
        };
        if pending.pane_id != pane_id || output_pane_id != pane_id {
            return Err(MezError::invalid_state(
                "focused-shell hook marker metadata does not match runtime dispatch state",
            ));
        }
        let success = exit_code == 0;
        let result = HookExecutionResult {
            hook_id: pending.plan.hook_id.clone(),
            event: pending.plan.event,
            status: if success {
                HookExecutionStatus::Succeeded
            } else {
                HookExecutionStatus::Failed
            },
            exit_code: Some(exit_code),
            stdout: format!("focused-shell hook exited with status {exit_code}"),
            stderr: String::new(),
            failure: if success {
                None
            } else {
                Some(HookFailure {
                    hook_id: pending.plan.hook_id.clone(),
                    event: pending.plan.event,
                    kind: HookFailureKind::ExitNonZero,
                    message: "focused-shell hook exited with non-zero status".to_string(),
                    retryable: false,
                })
            },
        };
        if !success {
            self.append_lifecycle_event(
                EventKind::HookFailed,
                format!(
                    r#"{{"hook_id":"{}","event":"{}","pane_id":"{}","exit_code":{},"marker":"{}"}}"#,
                    json_escape(&pending.plan.hook_id),
                    runtime_hook_event_name(pending.plan.event),
                    json_escape(pane_id),
                    exit_code,
                    json_escape(marker)
                ),
            )?;
        }
        if let Some(audit_log) = self.audit_log.as_mut() {
            let record = hook_execution_audit_record(
                &pending.plan,
                self.session.id.as_str(),
                AuditActor {
                    kind: "runtime".to_string(),
                    id: "focused-shell-hook-observer".to_string(),
                },
                "runtime_focused_shell_completion",
                &result,
            )
            .with_pane_id(pane_id.to_string());
            let _ = audit_log.append(record)?;
        }
        if let Some(continuation) = pending.continuation.as_ref() {
            let decision = self.record_hook_result(&pending.plan, &result, false)?;
            if decision == crate::hooks::HookFailureDecision::Block {
                let block = RuntimeHookPipelineBlock::from_result(&result);
                let _ = self.fail_pending_shell_action_for_hook_block(continuation, &block)?;
            } else {
                self.record_agent_pre_shell_hook_completed(continuation, &pending.plan.hook_id);
                let continuation_pane_id = self
                    .agent_turn_ledger
                    .turns()
                    .iter()
                    .find(|turn| turn.turn_id == continuation.turn_id)
                    .map(|turn| turn.pane_id.clone())
                    .unwrap_or_else(|| pane_id.to_string());
                self.append_agent_trace_turn_event(
                    &continuation_pane_id,
                    &continuation.turn_id,
                    &format!(
                        "action {} pre_shell_hook {} completed status={}",
                        continuation.action_id,
                        pending.plan.hook_id,
                        runtime_hook_execution_status_name(result.status)
                    ),
                )?;
                let _ = self.dispatch_stored_running_shell_actions(&continuation.turn_id)?;
            }
        }
        self.push_focused_shell_hook_result(result);
        Ok(1)
    }

    /// Runs the write active pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn write_active_pane_pipe(&mut self, pane_id: &str, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let Some(pipe) = self.active_pane_pipes.get_mut(pane_id) else {
            return Ok(());
        };
        if self.defer_file_pane_pipe_writes
            && let Some(path) = pipe.file_target_path()
        {
            pipe.record_deferred_output(bytes.len());
            self.deferred_pane_pipe_writes.push(DeferredPanePipeWrite {
                pane_id: pane_id.to_string(),
                path,
                bytes: bytes.to_vec(),
            });
            return Ok(());
        }
        let Err(error) = pipe.write_output(bytes) else {
            return Ok(());
        };
        let failure = error.message().to_string();
        let stopped = self.stop_active_pane_pipe(pane_id)?;
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","pipe":"stopped","mode":"{}","target":"{}","reason":"write-failed","bytes_written":{},"failure":"{}"}}"#,
                json_escape(&stopped.pane_id),
                stopped.mode,
                json_escape(&stopped.target),
                stopped.bytes_written,
                json_escape(&stopped.failure.unwrap_or(failure))
            ),
        )
    }

    /// Runs the start file pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_file_pane_pipe(
        &mut self,
        pane_id: String,
        path: PathBuf,
    ) -> Result<String> {
        let _ = self.stop_active_pane_pipe(pane_id.as_str());
        let pipe = if self.defer_file_pane_pipe_writes {
            ActivePanePipe::deferred_file(pane_id.clone(), path)
        } else {
            ActivePanePipe::file(pane_id.clone(), path)?
        };
        let body = format!(
            "target={}:pipe=started:mode={}:output={}:active_pipes={}",
            pipe.pane_id,
            pipe.mode(),
            pipe.target_label(),
            self.active_pane_pipes.len().saturating_add(1)
        );
        self.active_pane_pipes.insert(pane_id, pipe);
        Ok(body)
    }

    /// Runs the start command pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_command_pane_pipe(
        &mut self,
        pane_id: String,
        command: String,
    ) -> Result<String> {
        let _ = self.stop_active_pane_pipe(pane_id.as_str());
        let pipe = if self.defer_command_pane_pipe_startup {
            ActivePanePipe::deferred_command(pane_id.clone(), self.session.shell.path(), command)?
        } else {
            ActivePanePipe::command(pane_id.clone(), self.session.shell.path(), command)?
        };
        let body = format!(
            "target={}:pipe=started:mode={}:command={}:active_pipes={}",
            pipe.pane_id,
            pipe.mode(),
            pipe.target_label(),
            self.active_pane_pipes.len().saturating_add(1)
        );
        self.active_pane_pipes.insert(pane_id, pipe);
        Ok(body)
    }

    /// Runs the stop active pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop_active_pane_pipe(&mut self, pane_id: &str) -> Result<StoppedPanePipe> {
        let pipe = self.active_pane_pipes.remove(pane_id).ok_or_else(|| {
            MezError::new(crate::error::MezErrorKind::NotFound, "pane pipe not found")
        })?;
        Ok(pipe.stop())
    }

    /// Runs the stop active pane pipes for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop_active_pane_pipes_for(&mut self, pane_ids: &[&str]) -> Vec<StoppedPanePipe> {
        pane_ids
            .iter()
            .filter_map(|pane_id| {
                self.active_pane_pipes
                    .remove(*pane_id)
                    .map(ActivePanePipe::stop)
            })
            .collect()
    }

    /// Runs the stop all active pane pipes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop_all_active_pane_pipes(&mut self) -> Vec<StoppedPanePipe> {
        std::mem::take(&mut self.active_pane_pipes)
            .into_values()
            .map(ActivePanePipe::stop)
            .collect()
    }

    /// Returns whether the pane has a command-backed pipe that should be
    /// checked by the actor-owned health timer after accepted output.
    pub(crate) fn command_pane_pipe_health_check_needed(&self, pane_id: &str) -> Result<bool> {
        self.active_pane_pipes
            .get(pane_id)
            .map(|pipe| pipe.command_status())
            .transpose()
            .map(|status| status.flatten().is_some())
    }

    /// Returns pane ids that currently have command-backed pipes.
    pub(crate) fn active_command_pane_pipe_ids(&self) -> Vec<String> {
        self.active_pane_pipes
            .iter()
            .filter_map(|(pane_id, pipe)| match pipe.command_status() {
                Ok(Some(_)) => Some(pane_id.clone()),
                Ok(None) | Err(_) => None,
            })
            .collect()
    }

    /// Stops a command-backed pane pipe when its background command has exited
    /// or failed after accepting output.
    ///
    /// Command-pipe writers run outside actor state. A short actor-owned timer
    /// calls this after pane output is delivered so an asynchronously completed
    /// or failed command is reflected in pane state without waiting for a later
    /// pane-output write or explicit `pipe-pane --stop`.
    pub(crate) fn stop_completed_command_pane_pipe_for(&mut self, pane_id: &str) -> Result<usize> {
        let Some(status) = self
            .active_pane_pipes
            .get(pane_id)
            .map(|pipe| pipe.command_status())
            .transpose()?
            .flatten()
        else {
            return Ok(0);
        };
        if !status.completed && status.failure.is_none() {
            return Ok(0);
        }
        let stopped = self.stop_active_pane_pipe(pane_id)?;
        let reason = if stopped.failure.is_some() {
            "command-failed"
        } else {
            "command-completed"
        };
        let failure_json = stopped
            .failure
            .as_ref()
            .map(|failure| format!(r#","failure":"{}""#, json_escape(failure)))
            .unwrap_or_default();
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","pipe":"stopped","mode":"{}","target":"{}","reason":"{}","bytes_written":{}{}}}"#,
                json_escape(&stopped.pane_id),
                stopped.mode,
                json_escape(&stopped.target),
                reason,
                stopped.bytes_written,
                failure_json
            ),
        )?;
        Ok(1)
    }

    /// Stops active file-backed pane pipes that target the provided path.
    ///
    /// Async persistence failures arrive with a persistence path rather than a
    /// pane id. This helper reconciles that failure back into runtime pane-pipe
    /// state and emits one pane lifecycle event per stopped pipe.
    pub(crate) fn stop_file_pane_pipes_for_path(
        &mut self,
        path: &Path,
        reason: &str,
    ) -> Result<usize> {
        let pane_ids = self
            .active_pane_pipes
            .iter()
            .filter_map(|(pane_id, pipe)| {
                pipe.file_target_path()
                    .filter(|target_path| target_path == path)
                    .map(|_| pane_id.clone())
            })
            .collect::<Vec<_>>();
        let mut stopped_pipes = 0usize;
        for pane_id in pane_ids {
            let stopped = self.stop_active_pane_pipe(pane_id.as_str())?;
            self.append_lifecycle_event(
                EventKind::PaneChanged,
                format!(
                    r#"{{"pane_id":"{}","pipe":"stopped","mode":"{}","target":"{}","reason":"{}","bytes_written":{}}}"#,
                    json_escape(&stopped.pane_id),
                    stopped.mode,
                    json_escape(&stopped.target),
                    json_escape(reason),
                    stopped.bytes_written
                ),
            )?;
            stopped_pipes = stopped_pipes.saturating_add(1);
        }
        Ok(stopped_pipes)
    }

    /// Enables or disables deferred file pipe writes for async persistence.
    pub(crate) fn set_defer_file_pane_pipe_writes(&mut self, defer: bool) {
        self.defer_file_pane_pipe_writes = defer;
    }

    /// Enables or disables deferred command pipe startup for async runtimes.
    pub(crate) fn set_defer_command_pane_pipe_startup(&mut self, defer: bool) {
        self.defer_command_pane_pipe_startup = defer;
    }

    /// Drains file-backed pane pipe writes queued for async persistence.
    pub(crate) fn drain_deferred_pane_pipe_writes(&mut self) -> Vec<DeferredPanePipeWrite> {
        std::mem::take(&mut self.deferred_pane_pipe_writes)
    }

    /// Returns the user-facing active pane pipe status line used by
    /// `pipe-pane` and async actor tests that verify pipe lifecycle state.
    pub(crate) fn active_pane_pipe_display(&self) -> String {
        if self.active_pane_pipes.is_empty() {
            return "active_pipes=0".to_string();
        }
        self.active_pane_pipes
            .values()
            .map(|pipe| {
                format!(
                    "pane={}:mode={}:target={}:bytes={}",
                    pipe.pane_id,
                    pipe.mode(),
                    pipe.target_label(),
                    pipe.bytes_written
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Runs the start pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_pane_process(
        &mut self,
        descriptor: PaneDescriptor,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.start_pane_process_with_start_directory(descriptor, explicit_command, None)
    }

    /// Runs the start pane process with start directory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn start_pane_process_with_start_directory(
        &mut self,
        descriptor: PaneDescriptor,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        let environment = pane_environment_with_term(
            &self.socket_path,
            &self.session.id,
            &descriptor.window_id,
            &descriptor.pane_id,
            &self.terminal_term,
        )?;
        let shell = self.session.shell.clone();
        let primary_pid = self.pane_processes.spawn_for_pane_with_start_directory(
            descriptor.pane_id.as_str(),
            &shell,
            explicit_command,
            &environment,
            descriptor.size,
            start_directory,
        )?;
        self.pane_exit_records.remove(descriptor.pane_id.as_str());
        self.pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.terminal_history_limit,
                self.terminal_history_rotate_lines,
            )?,
        );
        self.pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.pane_bootstrap_pending
            .insert(descriptor.pane_id.to_string());
        if let Some(start_directory) = start_directory {
            self.pane_current_working_directories.insert(
                descriptor.pane_id.to_string(),
                start_directory.to_path_buf(),
            );
        }

        if shell.used_fallback() {
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"pane_id":"{}","diagnostic":"resolved shell fell back to /bin/sh"}}"#,
                    json_escape(descriptor.pane_id.as_str())
                ),
            )?;
        }

        let update = PaneProcessStart {
            session_id: self.session.id.to_string(),
            window_id: descriptor.window_id.to_string(),
            pane_id: descriptor.pane_id.to_string(),
            primary_pid,
            size: descriptor.size,
            registry_update: self.registry_update_plan(),
        };
        self.append_pane_start_event(&update)?;
        Ok(update)
    }

    /// Removes a live pane process from synchronous manager ownership for an
    /// async pane process owner.
    ///
    /// The session, screen, readiness, and lifecycle metadata stay in the
    /// runtime service; only PTY/process I/O ownership moves. Callers must start
    /// a replacement async owner before routing user input away from the
    /// compatibility manager path.
    pub fn take_running_pane_process_for_async_owner(
        &mut self,
        pane_id: &str,
    ) -> Result<PaneProcess> {
        self.require_live()?;
        let primary_pid = self.pane_processes.primary_pid(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })?;
        if let Some(current_working_directory) =
            self.pane_processes.current_working_directory(pane_id)
        {
            self.pane_current_working_directories
                .insert(pane_id.to_string(), current_working_directory);
        }
        let process = self.pane_processes.take_running_pane_process(pane_id)?;
        self.async_owned_pane_processes
            .insert(pane_id.to_string(), primary_pid);
        Ok(process)
    }

    /// Removes up to `limit` running pane processes for async pane workers.
    ///
    /// This is the dynamic production handoff entry point used by the async
    /// pane-process supervisor. Pane state remains in the runtime service while
    /// process, PTY output, input, resize, and termination ownership moves to
    /// one async worker per returned process.
    pub fn take_running_pane_processes_for_async_owner(
        &mut self,
        limit: usize,
    ) -> Result<Vec<(String, PaneProcess)>> {
        self.require_live()?;
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process handoff limit must be greater than zero",
            ));
        }
        let pane_ids = self
            .pane_processes
            .tracked_running_pane_ids()
            .into_iter()
            .take(limit)
            .collect::<Vec<_>>();
        let mut processes = Vec::with_capacity(pane_ids.len());
        for pane_id in pane_ids {
            let process = self.take_running_pane_process_for_async_owner(&pane_id)?;
            processes.push((pane_id, process));
        }
        Ok(processes)
    }

    /// Restores a pane process to synchronous manager ownership after a
    /// cancelled async owner handoff.
    pub fn restore_running_pane_process_from_async_owner(
        &mut self,
        pane_id: impl Into<String>,
        process: PaneProcess,
    ) -> Result<u32> {
        self.require_live()?;
        let pane_id = pane_id.into();
        self.async_owned_pane_processes.remove(&pane_id);
        self.pane_processes
            .insert_running_pane_process(pane_id, process)
    }

    /// Drains pane input operations deferred for async pane process workers.
    pub fn drain_deferred_pane_inputs(&mut self) -> Vec<DeferredPaneInput> {
        std::mem::take(&mut self.deferred_pane_inputs)
    }

    /// Drains coalesced pane resize operations deferred for async workers.
    pub fn drain_deferred_pane_resizes(&mut self) -> Vec<(String, DeferredPaneResize)> {
        std::mem::take(&mut self.deferred_pane_resizes)
            .into_iter()
            .collect()
    }

    /// Drains coalesced pane termination operations deferred for async workers.
    pub fn drain_deferred_pane_terminations(&mut self) -> Vec<(String, DeferredPaneTermination)> {
        std::mem::take(&mut self.deferred_pane_terminations)
            .into_iter()
            .collect()
    }

    /// Returns true when a pane's PTY/process handle is owned by an async worker.
    pub fn pane_process_is_async_owned(&self, pane_id: &str) -> bool {
        self.async_owned_pane_processes.contains_key(pane_id)
    }

    /// Runs the primary pid for live pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn primary_pid_for_live_pane_process(&self, pane_id: &str) -> Option<u32> {
        self.pane_processes
            .primary_pid(pane_id)
            .or_else(|| self.async_owned_pane_processes.get(pane_id).copied())
    }

    /// Writes pane input immediately when the synchronous manager still owns
    /// the pane, or records it for the async pane worker when ownership has
    /// moved across the actor boundary.
    pub(super) fn write_runtime_pane_input(&mut self, pane_id: &str, input: &[u8]) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, false)
    }

    /// Writes pane input with optional async queue priority.
    fn write_runtime_pane_input_with_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
        priority: bool,
    ) -> Result<()> {
        if input.is_empty() {
            return Err(MezError::invalid_args("pane input must not be empty"));
        }
        if self.pane_processes.contains_pane(pane_id) {
            return self.pane_processes.write_pane_input(pane_id, input);
        }
        if self.pane_process_is_async_owned(pane_id) {
            self.deferred_pane_inputs.push(DeferredPaneInput {
                pane_id: pane_id.to_string(),
                bytes: input.to_vec(),
                priority,
            });
            return Ok(());
        }
        Err(MezError::new(
            crate::error::MezErrorKind::NotFound,
            "pane process not found",
        ))
    }

    /// Writes pane input ahead of later queued input for the same async pane.
    pub(super) fn write_runtime_pane_input_priority(
        &mut self,
        pane_id: &str,
        input: &[u8],
    ) -> Result<()> {
        self.write_runtime_pane_input_with_priority(pane_id, input, true)
    }

    /// Terminates a pane process immediately when manager-owned, or queues a
    /// termination request for an async worker when ownership has moved.
    pub(super) fn terminate_runtime_pane_process(
        &mut self,
        pane_id: &str,
        force: bool,
    ) -> Result<bool> {
        self.agent_subshell_panes.remove(pane_id);
        self.agent_subshell_command_exit_panes.remove(pane_id);
        if self.pane_processes.contains_pane(pane_id) {
            return self
                .pane_processes
                .terminate_pane(pane_id)
                .map(|process| process.is_some());
        }
        if self.async_owned_pane_processes.remove(pane_id).is_some() {
            self.deferred_pane_terminations
                .insert(pane_id.to_string(), DeferredPaneTermination { force });
            return Ok(true);
        }
        Ok(false)
    }

    /// Terminates each listed pane process through the current owner boundary.
    pub(super) fn terminate_runtime_pane_processes<'a>(
        &mut self,
        pane_ids: impl IntoIterator<Item = &'a str>,
        force: bool,
    ) -> Result<usize> {
        let mut terminated = 0usize;
        for pane_id in pane_ids {
            if self.terminate_runtime_pane_process(pane_id, force)? {
                terminated = terminated.saturating_add(1);
            }
        }
        Ok(terminated)
    }

    /// Terminates all manager-owned and async-owned pane processes.
    pub(super) fn terminate_all_runtime_pane_processes(&mut self, force: bool) -> Result<usize> {
        let mut pane_ids = self.pane_processes.tracked_pane_ids();
        pane_ids.extend(self.async_owned_pane_processes.keys().cloned());
        self.terminate_runtime_pane_processes(pane_ids.iter().map(String::as_str), force)
    }

    /// Drops runtime-only state for a pane that has been removed from the
    /// session layout.
    ///
    /// Pane closure and process-exit paths remove the pane from the session
    /// model first, then call this helper to clear prompt, readiness, screen,
    /// deferred I/O, and subagent bookkeeping that would otherwise make a
    /// closed pane appear partially alive to later agent/session surfaces.
    pub(super) fn cleanup_removed_pane_runtime_state(&mut self, pane_id: &str) {
        self.agent_shell_store.remove_session(pane_id);
        self.agent_subshell_panes.remove(pane_id);
        self.agent_subshell_command_exit_panes.remove(pane_id);
        self.agent_prompt_inputs.remove(pane_id);
        self.agent_planning_modes.remove(pane_id);
        self.agent_personality_selections.remove(pane_id);
        self.agent_response_styles.remove(pane_id);
        self.agent_routing_overrides.remove(pane_id);
        self.agent_copy_outputs.remove(pane_id);
        self.agent_modified_files.remove(pane_id);
        self.active_copy_modes.remove(pane_id);
        self.pane_current_working_directories.remove(pane_id);
        self.deferred_pane_inputs
            .retain(|input| input.pane_id != pane_id);
        self.deferred_pane_resizes.remove(pane_id);
        self.deferred_pane_pipe_writes
            .retain(|write| write.pane_id != pane_id);
        self.pane_screens.remove(pane_id);
        self.pane_transaction_osc_screens.remove(pane_id);
        self.pane_transaction_osc_pending.remove(pane_id);
        self.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_commands.remove(pane_id);
        self.pane_mez_wrapper_filter_recent_polls.remove(pane_id);
        self.pane_hidden_shell_render_recent_polls.remove(pane_id);
        self.pane_exit_records.remove(pane_id);
        self.active_pane_pipes.remove(pane_id);
        self.pane_transcript_refs.remove(pane_id);
        self.pane_readiness_states.remove(pane_id);
        self.pane_readiness_overrides.clear_pending_probe(pane_id);
        self.pane_readiness_overrides
            .revoke(pane_id, ReadinessOverrideRevocation::PaneClosed);
        self.pane_environment_signatures.remove(pane_id);
        self.pane_bootstrap_pending.remove(pane_id);
        self.pane_instruction_files.remove(pane_id);
        self.pane_closing.remove(pane_id);
        self.pending_terminal_subagent_pane_closes.remove(pane_id);
        self.model_profile_overrides.pane_profiles.remove(pane_id);
        self.agent_auto_sizing_overrides.remove(pane_id);
        let pane_turn_ids = self
            .agent_turn_ledger
            .turns()
            .iter()
            .filter(|turn| turn.pane_id == pane_id)
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>();
        for turn_id in &pane_turn_ids {
            self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
            self.subagent_task_routes.remove(turn_id);
            self.clear_joined_subagent_dependencies_for_turn(turn_id);
        }

        let agent_id = format!("agent-{pane_id}");
        self.subagent_task_routes
            .retain(|_child_turn_id, parent_agent_id| parent_agent_id != &agent_id);
        self.joined_subagent_dependencies
            .retain(|_child_turn_id, dependency| dependency.child_agent_id != agent_id);
        self.model_profile_overrides
            .agent_profiles
            .remove(&agent_id);
        self.subagent_scopes.unregister(&agent_id);
        self.subagent_scope_declarations.remove(&agent_id);
        self.subagent_lineage.remove(&agent_id);
        if let Some(agent_id) = AgentId::opaque(agent_id)
            && self
                .message_service
                .registered_identity(&agent_id)
                .is_some()
        {
            let _ = self.message_service.update_presence(
                &agent_id,
                crate::message::AgentPresenceStatus::Offline,
                current_unix_seconds().saturating_mul(1000),
            );
        }

        let live_windows = self
            .session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect::<BTreeSet<_>>();
        self.subagent_window_ids
            .retain(|window_id| live_windows.contains(window_id));
    }

    /// Runs the close exited pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn close_exited_pane(&mut self, descriptor: &PaneDescriptor) -> Result<()> {
        self.pane_closing.remove(descriptor.pane_id.as_str());
        self.session
            .close_exited_pane(descriptor.pane_id.as_str())?;
        self.cleanup_removed_pane_runtime_state(descriptor.pane_id.as_str());
        Ok(())
    }

    /// Runs the initial pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn initial_pane_descriptor(&self) -> Result<PaneDescriptor> {
        let window = self
            .session
            .windows()
            .first()
            .ok_or_else(|| MezError::invalid_state("session has no windows"))?;
        let pane = window
            .panes()
            .first()
            .ok_or_else(|| MezError::invalid_state("initial window has no panes"))?;
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        Ok(PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        })
    }

    /// Runs the active window pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn active_window_pane_descriptor(
        &self,
        target: Option<&str>,
    ) -> Result<PaneDescriptor> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let pane = match target {
            Some(target) => window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == target || pane.index.to_string() == target)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
                })?,
            None => window.active_pane(),
        };
        let size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        Ok(PaneDescriptor {
            window_id: window.id.clone(),
            pane_id: pane.id.clone(),
            size,
        })
    }

    /// Returns the pane descriptors that should receive primary input.
    pub(super) fn active_window_input_descriptors(&self) -> Result<Vec<PaneDescriptor>> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        let panes = if self.session.active_window_panes_synchronized() {
            window.panes().iter().collect::<Vec<_>>()
        } else {
            vec![window.active_pane()]
        };
        Ok(panes
            .into_iter()
            .map(|pane| PaneDescriptor {
                window_id: window.id.clone(),
                pane_id: pane.id.clone(),
                size: self
                    .pane_process_size_for(window, pane.id.as_str())
                    .unwrap_or(pane.size),
            })
            .collect())
    }

    /// Runs the find pane descriptor operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn find_pane_descriptor(&self, pane_id: &str) -> Option<PaneDescriptor> {
        self.session.windows().iter().find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| PaneDescriptor {
                    window_id: window.id.clone(),
                    pane_id: pane.id.clone(),
                    size: self
                        .pane_process_size_for(window, pane.id.as_str())
                        .unwrap_or(pane.size),
                })
        })
    }

    /// Runs the find pane title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn find_pane_title(&self, pane_id: &str) -> Option<String> {
        self.session.windows().iter().find_map(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id.as_str() == pane_id)
                .map(|pane| pane.title.clone())
        })
    }

    /// Runs the tracked pane descriptors operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn tracked_pane_descriptors(&self) -> Vec<PaneDescriptor> {
        self.session
            .windows()
            .iter()
            .flat_map(|window| {
                window.panes().iter().filter_map(|pane| {
                    if self.pane_processes.contains_pane(pane.id.as_str())
                        || self.pane_process_is_async_owned(pane.id.as_str())
                    {
                        let size = self
                            .pane_process_size_for(window, pane.id.as_str())
                            .unwrap_or(pane.size);
                        Some(PaneDescriptor {
                            window_id: window.id.clone(),
                            pane_id: pane.id.clone(),
                            size,
                        })
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Runs the pane process size for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn pane_process_size_for(&self, window: &crate::layout::Window, pane_id: &str) -> Option<Size> {
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let window_frame_visible = self.window_frames_enabled;
        let group_rows = u16::from(self.session.window_groups().len() > 1);
        let display_size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_rows).max(1),
        )
        .ok()?;
        if window.zoomed_pane_id() == Some(&pane.id) {
            let body_size = rendered_window_body_size(display_size, window_frame_visible).ok()?;
            let geometry = crate::layout::PaneGeometry {
                index: pane.index,
                column: 0,
                row: 0,
                columns: body_size.columns,
                rows: body_size.rows,
            };
            let content_size = pane_content_size_for_geometry(
                &geometry,
                std::slice::from_ref(&geometry),
                self.pane_frames_enabled,
                self.pane_frame_position,
            )
            .ok()?;
            return Some(
                self.pane_process_size_after_agent_prompt_reservation(
                    pane.id.as_str(),
                    content_size,
                ),
            );
        }

        let body_size = rendered_window_body_size(display_size, window_frame_visible).ok()?;
        let geometries = window.pane_geometries_for_size(body_size);
        let geometry = geometries
            .iter()
            .find(|geometry| geometry.index == pane.index)?;
        let content_size = pane_content_size_for_geometry(
            geometry,
            &geometries,
            self.pane_frames_enabled,
            self.pane_frame_position,
        )
        .ok()?;
        Some(self.pane_process_size_after_agent_prompt_reservation(pane.id.as_str(), content_size))
    }

    /// Removes rows reserved for the pane-local agent prompt from the PTY size
    /// advertised to the shell. Keeping the process size aligned with the
    /// visible terminal buffer prevents prompts and cursor reports from
    /// landing underneath the agent input row.
    fn pane_process_size_after_agent_prompt_reservation(&self, pane_id: &str, size: Size) -> Size {
        let reserved_rows = self.agent_prompt_reserved_rows_for_pane(
            pane_id,
            usize::from(size.columns),
            usize::from(size.rows),
        );
        let reserved_rows = u16::try_from(reserved_rows)
            .unwrap_or(u16::MAX)
            .min(size.rows.saturating_sub(1));
        Size {
            columns: size.columns,
            rows: size.rows.saturating_sub(reserved_rows).max(1),
        }
    }

    /// Runs the append pane start event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_start_event(&mut self, update: &PaneProcessStart) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","columns":{},"rows":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.size.columns,
                update.size.rows
            ),
        )
    }

    /// Runs the append pane resize event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_resize_event(&mut self, update: &PaneResizeUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","layout":"resized","columns":{},"rows":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.size.columns,
                update.size.rows
            ),
        )
    }

    /// Runs the append pane output event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_output_event(&mut self, update: &PaneOutputUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","output_bytes":{},"activity_events":{},"bell_events":{},"background":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.bytes_read,
                update.activity_events,
                update.bell_events,
                update.background
            ),
        )
    }

    /// Runs the append pane title event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_title_event(&mut self, update: &PaneOutputUpdate) -> Result<()> {
        let title = self
            .find_pane_title(update.pane_id.as_str())
            .unwrap_or_else(|| "shell".to_string());
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","title":"{}"}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                json_escape(&title)
            ),
        )
    }

    /// Runs the append pane exit event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_exit_event(&mut self, update: &PaneExitUpdate) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"exited","exit_status":{},"exit_code":{},"signal":{},"closed_window":{},"session_empty":{}}}"#,
                json_escape(&update.pane_id),
                json_escape(&update.window_id),
                update.primary_pid,
                update.exit_status.to_json(),
                optional_i32_json(update.exit_status.code),
                optional_i32_json(update.exit_status.signal),
                update.closed_window,
                update.session_empty
            ),
        )
    }

    /// Runs the append pane close event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_pane_close_event(
        &mut self,
        pane_id: &str,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::PaneChanged,
            format!(
                r#"{{"pane_id":"{}","window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
                json_escape(pane_id),
                json_escape(window_id),
                terminated_panes,
                session_empty
            ),
        )
    }

    /// Runs the append window close event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_window_close_event(
        &mut self,
        window_id: &str,
        terminated_panes: usize,
        session_empty: bool,
    ) -> Result<()> {
        self.append_lifecycle_event(
            EventKind::WindowChanged,
            format!(
                r#"{{"window_id":"{}","state":"closed","closed":true,"terminated_panes":{},"session_empty":{}}}"#,
                json_escape(window_id),
                terminated_panes,
                session_empty
            ),
        )
    }
}
