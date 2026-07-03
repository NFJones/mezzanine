//! Runtime Processes implementation.
//!
//! This module owns the runtime processes boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.
mod layout;
pub(crate) mod output_filter;
mod pane_pipes;
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
        let live_primary_pid = self.primary_pid_for_live_pane_process(&pane_id);
        if primary_pid != 0
            && let Some(live_primary_pid) = live_primary_pid
            && live_primary_pid != primary_pid
        {
            return Ok(None);
        }
        let primary_pid = if primary_pid == 0 {
            live_primary_pid.unwrap_or(0)
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
            if self.find_pane_descriptor(&output.pane_id).is_none() {
                continue;
            }
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
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Ok(None);
        }
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
        self.agent_local_action_executor_overrides.remove(pane_id);
        self.agent_routing_overrides.remove(pane_id);
        self.agent_copy_outputs.remove(pane_id);
        self.agent_modified_files.remove(pane_id);
        self.active_copy_modes.remove(pane_id);
        self.pane_current_working_directories.remove(pane_id);
        self.pane_foreground_process_groups.remove(pane_id);
        self.program_owned_pane_titles.remove(pane_id);
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
