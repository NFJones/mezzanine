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
use mez_mux::presentation::{pane_content_size_for_geometry, rendered_window_body_size};

use super::{
    ActionContentBlock, ActionResult, ActionStatus, ActivePanePipe, AgentId, AgentTurnRecord,
    AgentTurnState, AuditActor, BTreeSet, CommandInvocation, CommandOutcome, ContextBlock,
    ContextSourceKind, EnvironmentSignature, EventKind, ExitedPaneProcess, HookEvent,
    HookExecutionResult, HookExecutionStatus, HookFailure, HookFailureKind, MezError,
    PaneDescriptor, PaneExitRecord, PaneExitStatus, PaneExitUpdate, PaneId, PaneOutputUpdate,
    PaneProcessManager, PaneProcessOutput, PaneProcessStart, PaneReadinessState, PaneResizeUpdate,
    PaneSizeSpec, Path, PathBuf, ReadinessOverrideRevocation, ResizeAxis, ResizeDirection, Result,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeHookPipelineBlock,
    RuntimeLifecycleState, RuntimeSessionService, RuntimeShellTransactionActionFailure,
    RuntimeShellTransactionTimerKind, RuntimeShellTransactionTimerRef, SessionSnapshotPayload,
    ShellClassification, ShellTransaction, Size, SplitDirection, StoppedPanePipe, TerminalOscEvent,
    TerminalScreen, WindowId, action_result_context_content, current_unix_millis,
    current_unix_seconds, decode_shell_output_transport_with_diagnostics,
    execute_mark_pane_ready_command, focused_shell_pre_action_timeout_result,
    hook_execution_audit_record, json_escape, local_action_plan, optional_i32_json,
    pane_environment_with_term, postprocess_shell_action_success_output,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_execution_ready_for_provider_continuation, runtime_hook_event_name,
    runtime_hook_execution_status_name, runtime_marker_for_action,
    runtime_pane_readiness_state_name, runtime_post_shell_hook_payload,
    runtime_random_marker_token, shell_command_result_content, validate_pane_size_for_resize,
};
use crate::runtime::service_state::ProgramOwnedPaneTitle;
use crate::runtime::{
    PaneEvent, ProcessEvent, RenderInvalidationReason, RuntimeSideEffect, RuntimeTransition,
};
use crate::terminal::parse_mez_shell_transaction_osc;
use mez_agent::AgentActionPayload;
use mez_agent::semantic_patch_planning::{
    ApplyPatchTransactionPhase, apply_patch_transaction_phase,
};
use mez_agent::shell_observation::{
    agent_shell_transaction_bytes_before_end_marker, agent_shell_transaction_observation_bytes,
    find_byte_subsequence, latest_agent_shell_transaction_output_lines,
    mez_wrapper_echo_line_is_hidden, mez_wrapper_echo_line_is_possible_prefix,
    mez_wrapper_echo_line_visible_bytes, mez_wrapper_filter_bytes_may_contain_boilerplate,
    renderable_shell_transaction_bytes,
};
use mez_agent::{
    DEFAULT_BOOTSTRAP_TIMEOUT_MS, bootstrap_script_for_classification, parse_bootstrap_env_output,
    readiness_probe_command_for_classification,
};
use mez_mux::process::PaneProcess;
use mez_terminal::TerminalStyledLine;

use transactions::{
    RUNTIME_HIDDEN_SHELL_RENDER_RETENTION_POLLS, RUNTIME_MEZ_OSC_PREFIX,
    RUNTIME_MEZ_OSC_SCAN_LIMIT_BYTES, RUNTIME_SHELL_WRAPPER_FILTER_RECENT_COMMAND_LIMIT,
    RUNTIME_SHELL_WRAPPER_FILTER_RETENTION_POLLS, runtime_running_shell_transaction_kind_name,
};

/// Owns live process metadata that is private to the pane process subsystem.
///
/// Detached process ids, observed foreground groups, and program-owned title
/// lifetimes change together with pane process events. Keeping them behind
/// this component prevents unrelated runtime leaves from mutating incomplete
/// process metadata.
#[derive(Debug, Default)]
pub(in crate::runtime) struct RuntimeProcessComponent {
    /// Live terminal and shell settings applied to process state.
    settings: RuntimeProcessSettings,
    /// Live pane process handles and their PTY lifecycle manager.
    pane_processes: PaneProcessManager,
    /// Best-known current working directory for each pane process.
    pane_current_working_directories: std::collections::BTreeMap<String, PathBuf>,
    /// Latest readiness state observed for each pane shell.
    pane_readiness_states: std::collections::BTreeMap<String, PaneReadinessState>,
    /// Explicit readiness overrides and pending probe epochs.
    pane_readiness_overrides: mez_agent::PaneReadinessOverrideStore,
    /// Bootstrap-derived environment signatures keyed by pane id.
    pane_environment_signatures: std::collections::BTreeMap<String, EnvironmentSignature>,
    /// Panes with an in-flight bootstrap transaction.
    pane_bootstrap_pending: BTreeSet<String>,
    /// Modeled terminal screen state keyed by pane id.
    pane_screens: std::collections::BTreeMap<String, TerminalScreen>,
    /// Live shell transactions keyed by their OSC marker.
    running_shell_transactions: std::collections::BTreeMap<String, RunningShellTransactionRef>,
    /// Markers whose runtime wrappers must emit start before completion.
    shell_transaction_require_start_markers: BTreeSet<String>,
    /// Markers whose mandatory wrapper start event has been observed.
    shell_transaction_started_markers: BTreeSet<String>,
    /// Active pane output pipes keyed by their source pane id.
    active_pane_pipes: std::collections::BTreeMap<String, ActivePanePipe>,
    /// Primary process ids for panes whose handles are adapter-owned.
    detached_pane_primary_pids: std::collections::BTreeMap<String, u32>,
    /// Latest foreground process groups observed by pane workers.
    pane_foreground_process_groups: std::collections::BTreeMap<String, u32>,
    /// Program-owned pane title state keyed by pane id.
    program_owned_pane_titles: std::collections::BTreeMap<String, ProgramOwnedPaneTitle>,
    /// Full terminal parsers retained for visible shell transaction streams.
    pane_transaction_osc_screens: std::collections::BTreeMap<String, TerminalScreen>,
    /// Bounded hidden-shell OSC marker fragments keyed by pane id.
    pane_transaction_osc_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Partial wrapper-filter bytes keyed by pane id.
    pane_mez_wrapper_filter_pending: std::collections::BTreeMap<String, Vec<u8>>,
    /// Recently hidden wrapper commands keyed by pane id.
    pane_mez_wrapper_filter_recent_commands: std::collections::BTreeMap<String, Vec<String>>,
    /// Remaining wrapper-filter retention polls keyed by pane id.
    pane_mez_wrapper_filter_recent_polls: std::collections::BTreeMap<String, usize>,
    /// Remaining hidden-shell render retention polls keyed by pane id.
    pane_hidden_shell_render_recent_polls: std::collections::BTreeMap<String, usize>,
    /// Consecutive idle polls used to synchronize foreground titles.
    foreground_title_idle_sync_polls: usize,
    /// Terminal exit records retained for panes whose primary process ended.
    pane_exit_records: std::collections::BTreeMap<String, PaneExitRecord>,
    /// Panes whose process teardown has begun but is not yet fully reconciled.
    pane_closing: BTreeSet<String>,
}

/// Owns terminal configuration that controls pane process and screen behavior.
///
/// These values are parsed together during config application and must be
/// replaced together so newly spawned screens, existing history buffers, and
/// process environments observe one coherent settings generation.
#[derive(Debug, Clone)]
struct RuntimeProcessSettings {
    /// Maximum retained history lines for each pane screen.
    terminal_history_limit: usize,
    /// History lines removed in each overflow rotation batch.
    terminal_history_rotate_lines: usize,
    /// TERM value exported to pane processes and attached clients.
    terminal_term: String,
    /// Hidden shell output tail lines retained in action previews.
    terminal_shell_output_preview_lines: usize,
}

impl Default for RuntimeProcessSettings {
    fn default() -> Self {
        Self {
            terminal_history_limit: mez_terminal::DEFAULT_HISTORY_LIMIT,
            terminal_history_rotate_lines: mez_terminal::DEFAULT_HISTORY_ROTATE_LINES,
            terminal_term: mez_terminal::DEFAULT_PANE_TERM.to_string(),
            terminal_shell_output_preview_lines: 5,
        }
    }
}

impl RuntimeProcessComponent {
    /// Builds process ownership around the manager supplied by runtime construction.
    pub(in crate::runtime) fn with_pane_processes(pane_processes: PaneProcessManager) -> Self {
        Self {
            pane_processes,
            ..Self::default()
        }
    }
}

impl RuntimeSessionService {
    /// Returns the number of active pane output pipes.
    pub(in crate::runtime) fn active_pane_pipe_count(&self) -> usize {
        self.process.active_pane_pipes.len()
    }

    /// Registers one live shell transaction and its start-marker invariant.
    pub(in crate::runtime) fn register_running_shell_transaction(
        &mut self,
        marker: String,
        transaction: RunningShellTransactionRef,
        require_start_marker: bool,
    ) {
        self.process
            .running_shell_transactions
            .insert(marker.clone(), transaction);
        if require_start_marker {
            self.process
                .shell_transaction_require_start_markers
                .insert(marker);
        }
    }

    /// Reports whether an agent action has a live shell transaction.
    pub(in crate::runtime) fn agent_action_has_running_shell_transaction(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| {
                transaction.turn_id == turn_id
                    && matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::AgentAction {
                            action_id: running_action_id
                        } if running_action_id == action_id
                    )
            })
    }

    /// Reports whether a turn has any live agent-action shell transaction.
    pub(in crate::runtime) fn turn_has_running_agent_action_shell_transaction(
        &self,
        turn_id: &str,
    ) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| {
                transaction.turn_id == turn_id
                    && matches!(
                        transaction.kind,
                        RunningShellTransactionKind::AgentAction { .. }
                    )
            })
    }

    /// Reports whether a turn has a live transaction of the requested kind.
    pub(in crate::runtime) fn turn_has_running_shell_transaction_kind(
        &self,
        turn_id: &str,
        kind: &RunningShellTransactionKind,
    ) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.turn_id == turn_id && &transaction.kind == kind)
    }

    /// Reports whether one pane has any live shell transaction.
    pub(in crate::runtime) fn pane_has_running_shell_transaction(&self, pane_id: &str) -> bool {
        self.process
            .running_shell_transactions
            .values()
            .any(|transaction| transaction.pane_id == pane_id)
    }

    /// Returns marker and pane pairs for every live transaction in one turn.
    pub(in crate::runtime) fn running_shell_transaction_targets_for_turn(
        &self,
        turn_id: &str,
    ) -> Vec<(String, String)> {
        self.process
            .running_shell_transactions
            .iter()
            .filter(|(_, transaction)| transaction.turn_id == turn_id)
            .map(|(marker, transaction)| (marker.clone(), transaction.pane_id.clone()))
            .collect()
    }

    /// Removes one live shell transaction by marker.
    pub(in crate::runtime) fn remove_running_shell_transaction(
        &mut self,
        marker: &str,
    ) -> Option<RunningShellTransactionRef> {
        self.process.running_shell_transactions.remove(marker)
    }

    /// Clears all live shell transactions and marker protocol state.
    pub(in crate::runtime) fn clear_all_shell_transaction_state(&mut self) {
        self.process.running_shell_transactions.clear();
        self.process.shell_transaction_require_start_markers.clear();
        self.process.shell_transaction_started_markers.clear();
    }

    /// Returns the active pane-screen history limit.
    pub(crate) fn terminal_history_limit(&self) -> usize {
        self.process.settings.terminal_history_limit
    }

    /// Returns the active pane-screen history rotation batch size.
    pub(in crate::runtime) fn terminal_history_rotate_lines(&self) -> usize {
        self.process.settings.terminal_history_rotate_lines
    }

    /// Returns the TERM value exported to pane processes and clients.
    pub(in crate::runtime) fn terminal_term(&self) -> &str {
        &self.process.settings.terminal_term
    }

    /// Applies one parsed generation of terminal process settings.
    pub(in crate::runtime) fn apply_process_terminal_settings(
        &mut self,
        history_limit: usize,
        history_rotate_lines: usize,
        terminal_term: String,
        terminal_emoji_width: mez_terminal::TerminalEmojiWidth,
        shell_output_preview_lines: usize,
    ) -> Result<()> {
        self.configure_pane_screen_history(history_limit, history_rotate_lines)?;
        self.process.settings = RuntimeProcessSettings {
            terminal_history_limit: history_limit,
            terminal_history_rotate_lines: history_rotate_lines,
            terminal_term,
            terminal_shell_output_preview_lines: shell_output_preview_lines,
        };
        mez_terminal::set_terminal_emoji_width(terminal_emoji_width);
        Ok(())
    }

    /// Returns all modeled pane screens for whole-layout presentation.
    pub(in crate::runtime) fn pane_screens(
        &self,
    ) -> &std::collections::BTreeMap<String, TerminalScreen> {
        &self.process.pane_screens
    }

    /// Returns the modeled terminal screen for one pane.
    pub(crate) fn pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.process.pane_screens.get(pane_id)
    }

    /// Returns mutable modeled terminal state for one runtime operation.
    pub(in crate::runtime) fn pane_screen_mut(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut TerminalScreen> {
        self.process.pane_screens.get_mut(pane_id)
    }

    /// Replaces the modeled terminal screen for one pane.
    pub(in crate::runtime) fn set_pane_screen(
        &mut self,
        pane_id: impl Into<String>,
        screen: TerminalScreen,
    ) {
        self.process.pane_screens.insert(pane_id.into(), screen);
    }

    /// Clears modeled terminal state when the live session is replaced.
    pub(in crate::runtime) fn clear_pane_screens(&mut self) {
        self.process.pane_screens.clear();
    }

    /// Applies new history retention policy to every modeled pane screen.
    pub(in crate::runtime) fn configure_pane_screen_history(
        &mut self,
        history_limit: usize,
        rotate_lines: usize,
    ) -> Result<()> {
        for screen in self.process.pane_screens.values_mut() {
            screen.set_history_limit(history_limit)?;
            screen.set_history_rotate_lines(rotate_lines)?;
        }
        Ok(())
    }

    /// Returns the last readiness state observed for a pane shell.
    pub(in crate::runtime) fn pane_readiness_state(&self, pane_id: &str) -> PaneReadinessState {
        self.process
            .pane_readiness_states
            .get(pane_id)
            .copied()
            .unwrap_or(PaneReadinessState::Unknown)
    }

    /// Records the current readiness state for one pane shell.
    pub(in crate::runtime) fn set_pane_readiness(
        &mut self,
        pane_id: &str,
        state: PaneReadinessState,
    ) {
        self.process
            .pane_readiness_states
            .insert(pane_id.to_string(), state);
    }

    /// Revokes readiness authority for one pane after a shell lifecycle event.
    pub(in crate::runtime) fn revoke_pane_readiness_override(
        &mut self,
        pane_id: &str,
        reason: ReadinessOverrideRevocation,
    ) {
        self.process
            .pane_readiness_overrides
            .revoke(pane_id, reason);
    }

    /// Reports whether one pane still has a readiness probe in flight.
    pub(in crate::runtime) fn pane_readiness_override_has_pending_probe(
        &self,
        pane_id: &str,
    ) -> bool {
        self.process
            .pane_readiness_overrides
            .has_pending_probe(pane_id)
    }

    /// Executes the readiness override command against process-owned state.
    pub(in crate::runtime) fn execute_pane_readiness_override_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        invocation: &CommandInvocation,
        current_state: PaneReadinessState,
        current_epoch: u64,
    ) -> Result<CommandOutcome> {
        execute_mark_pane_ready_command(
            &self.session,
            primary_client_id,
            &mut self.process.pane_readiness_overrides,
            invocation,
            current_state,
            current_epoch,
            self.audit_log.as_mut(),
        )
    }

    /// Returns the bootstrap-derived environment signature for one pane.
    pub(in crate::runtime) fn pane_environment_signature(
        &self,
        pane_id: &str,
    ) -> Option<&EnvironmentSignature> {
        self.process.pane_environment_signatures.get(pane_id)
    }

    /// Clears pane readiness states and manual overrides for session replacement.
    pub(in crate::runtime) fn clear_pane_readiness_state_and_overrides(&mut self) {
        self.process.pane_readiness_states.clear();
        self.process.pane_readiness_overrides = Default::default();
    }

    /// Records the best-known current working directory for one pane process.
    pub(in crate::runtime) fn set_pane_current_working_directory(
        &mut self,
        pane_id: impl Into<String>,
        path: PathBuf,
    ) {
        self.process
            .pane_current_working_directories
            .insert(pane_id.into(), path);
    }

    /// Terminates every process still owned by the runtime.
    pub(crate) fn terminate_all_pane_processes(&mut self) -> Result<Vec<ExitedPaneProcess>> {
        Ok(self.process.pane_processes.terminate_all()?)
    }

    /// Returns the current output-activity sequence for one pane process.
    pub(in crate::runtime) fn pane_process_output_activity_sequence(
        &self,
        pane_id: &str,
    ) -> Option<u64> {
        self.process
            .pane_processes
            .output_activity_sequence(pane_id)
    }

    /// Waits for a pane process to publish output after a known sequence.
    pub(in crate::runtime) fn wait_for_pane_process_output_activity_after(
        &self,
        pane_id: &str,
        sequence: u64,
        timeout: std::time::Duration,
    ) -> Option<bool> {
        self.process
            .pane_processes
            .wait_for_output_activity_after(pane_id, sequence, timeout)
    }

    /// Returns the executable name observed for one live pane process.
    pub(in crate::runtime) fn pane_process_name(&self, pane_id: &str) -> Option<String> {
        self.process.pane_processes.process_name(pane_id)
    }

    /// Returns pane ids currently tracked by the live process manager.
    pub(in crate::runtime) fn tracked_runtime_pane_process_ids(&self) -> Vec<String> {
        self.process.pane_processes.tracked_pane_ids()
    }

    /// Clears visible and hidden shell transaction parser state on shutdown.
    pub(in crate::runtime) fn clear_pane_transaction_parsers(&mut self) {
        self.process.pane_transaction_osc_screens.clear();
        self.process.pane_transaction_osc_pending.clear();
    }

    /// Clears pane exit and closing markers when the live session is replaced.
    pub(in crate::runtime) fn clear_pane_process_lifecycle_tracking(&mut self) {
        self.process.pane_exit_records.clear();
        self.process.pane_closing.clear();
    }

    /// Returns the last observed exit status for a pane process.
    pub(in crate::runtime) fn pane_exit_status(&self, pane_id: &str) -> Option<PaneExitStatus> {
        self.process
            .pane_exit_records
            .get(pane_id)
            .map(|record| record.exit_status)
    }

    /// Marks a pane as being in process teardown.
    pub(in crate::runtime) fn mark_pane_closing(&mut self, pane_id: impl Into<String>) {
        self.process.pane_closing.insert(pane_id.into());
    }

    /// Reports whether a pane is already in process teardown.
    pub(in crate::runtime) fn pane_is_closing(&self, pane_id: &str) -> bool {
        self.process.pane_closing.contains(pane_id)
    }
}

#[cfg(test)]
impl RuntimeSessionService {
    /// Returns live shell transactions for integration-test observation.
    pub(in crate::runtime) fn running_shell_transactions_for_tests(
        &self,
    ) -> &std::collections::BTreeMap<String, RunningShellTransactionRef> {
        &self.process.running_shell_transactions
    }

    /// Returns live shell transactions for process-fixture mutation.
    pub(in crate::runtime) fn running_shell_transactions_mut_for_tests(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, RunningShellTransactionRef> {
        &mut self.process.running_shell_transactions
    }

    /// Reports whether a transaction still requires a start marker.
    pub(in crate::runtime) fn shell_transaction_requires_start_marker_for_tests(
        &self,
        marker: &str,
    ) -> bool {
        self.process
            .shell_transaction_require_start_markers
            .contains(marker)
    }

    /// Reports whether a transaction start marker has been observed.
    pub(in crate::runtime) fn shell_transaction_started_for_tests(&self, marker: &str) -> bool {
        self.process
            .shell_transaction_started_markers
            .contains(marker)
    }
    /// Installs a manual readiness override for a test epoch.
    pub(in crate::runtime) fn mark_pane_readiness_override_for_tests(
        &mut self,
        pane_id: &str,
        epoch: u64,
        reason: &str,
        one_shot: bool,
    ) -> Result<()> {
        self.process
            .pane_readiness_overrides
            .mark_ready_for_epoch(pane_id, epoch, reason, one_shot)?;
        Ok(())
    }

    /// Reports whether a manual readiness override allows a test epoch.
    pub(in crate::runtime) fn pane_readiness_override_allows_epoch_for_tests(
        &self,
        pane_id: &str,
        epoch: u64,
    ) -> bool {
        self.process
            .pane_readiness_overrides
            .allows_epoch(pane_id, epoch)
    }

    /// Reports whether bootstrap remains pending for a process fixture.
    pub(in crate::runtime) fn pane_bootstrap_is_pending_for_tests(&self, pane_id: &str) -> bool {
        self.process.pane_bootstrap_pending.contains(pane_id)
    }
    /// Returns the process manager for integration-test observation.
    pub(crate) fn pane_processes(&self) -> &PaneProcessManager {
        &self.process.pane_processes
    }

    /// Returns the process manager for test-only process-fixture mutation.
    pub(crate) fn pane_processes_mut(&mut self) -> &mut PaneProcessManager {
        &mut self.process.pane_processes
    }

    /// Returns mutable visible transaction parsers for a process fixture.
    pub(in crate::runtime) fn pane_transaction_osc_screens_mut_for_tests(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, TerminalScreen> {
        &mut self.process.pane_transaction_osc_screens
    }

    /// Returns visible transaction parsers for process integration tests.
    pub(in crate::runtime) fn pane_transaction_osc_screens_for_tests(
        &self,
    ) -> &std::collections::BTreeMap<String, TerminalScreen> {
        &self.process.pane_transaction_osc_screens
    }

    /// Returns hidden transaction fragments for process integration tests.
    pub(in crate::runtime) fn pane_transaction_osc_pending_for_tests(
        &self,
    ) -> &std::collections::BTreeMap<String, Vec<u8>> {
        &self.process.pane_transaction_osc_pending
    }

    /// Installs one pane exit status for presentation integration tests.
    pub(in crate::runtime) fn set_pane_exit_status_for_tests(
        &mut self,
        pane_id: impl Into<String>,
        exit_status: PaneExitStatus,
    ) {
        self.process
            .pane_exit_records
            .insert(pane_id.into(), PaneExitRecord { exit_status });
    }
}

// Pane process lifecycle and PTY synchronization.

impl RuntimeSessionService {
    /// Runs the shell classification for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn shell_classification_for_pane(&self, pane_id: &str) -> ShellClassification {
        self.process
            .pane_environment_signatures
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
        let exited = self.process.pane_processes.poll_exited()?;
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
            self.process.detached_pane_primary_pids.remove(&pane_id);
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
            .or_else(|| {
                self.process
                    .pane_processes
                    .primary_pid(descriptor.pane_id.as_str())
            })
            .unwrap_or(0);
        self.process
            .pane_exit_records
            .remove(descriptor.pane_id.as_str());
        self.session
            .set_pane_live_state(descriptor.pane_id.as_str(), true)?;
        self.process.pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process
            .pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.process
            .pane_bootstrap_pending
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

    /// Applies one process lifecycle event through the transport-neutral transition contract.
    pub(crate) fn apply_process_transition(
        &mut self,
        event: ProcessEvent,
    ) -> Result<RuntimeTransition> {
        let (applied, render_reason) = match event {
            ProcessEvent::Exited {
                pane_id,
                primary_pid,
                exit_code,
                signal,
            } => {
                let primary_pid = primary_pid
                    .or_else(|| self.process.pane_processes.primary_pid(&pane_id))
                    .unwrap_or(0);
                let signal_number = signal
                    .as_deref()
                    .and_then(|signal| signal.parse::<i32>().ok());
                let status = PaneExitStatus {
                    code: exit_code,
                    signal: signal_number,
                    success: exit_code == Some(0) && signal.is_none(),
                };
                (
                    self.apply_pane_process_exit_event(pane_id, primary_pid, status)?
                        .is_some(),
                    Some(RenderInvalidationReason::Layout),
                )
            }
            ProcessEvent::Failed { pane_id, error } => (
                self.apply_pane_process_failure_event(pane_id, error)?,
                Some(RenderInvalidationReason::FullRedraw),
            ),
            ProcessEvent::Spawned { pane_id, pid } => (
                self.apply_pane_process_spawn_event(pane_id, pid)?,
                Some(RenderInvalidationReason::FullRedraw),
            ),
        };
        let mut transition = self.runtime_transition_with_render(applied, render_reason);
        if applied {
            transition
                .side_effects
                .extend(self.registry_persistence_transition().side_effects);
        }
        Ok(transition)
    }

    /// Applies one non-output pane event through the transport-neutral transition contract.
    ///
    /// Pane output remains actor-owned temporarily because it also updates ingress metrics and
    /// pane-pipe health timers. Completion events can already return their ordered render effects
    /// without depending on Tokio or transport state.
    pub(crate) fn apply_pane_completion_transition(
        &mut self,
        event: PaneEvent,
    ) -> Result<RuntimeTransition> {
        let (applied, render_reason) = match event {
            PaneEvent::WriteFailed { pane_id, error } => (
                self.apply_pane_write_failure_event(pane_id, error)?,
                Some(RenderInvalidationReason::FullRedraw),
            ),
            PaneEvent::Resized { pane_id, size } => (
                self.apply_pane_resize_completion_event(pane_id, size)?,
                Some(RenderInvalidationReason::Layout),
            ),
            PaneEvent::ForegroundProcess {
                pane_id,
                process_name,
                process_group_id,
                current_working_directory,
            } => (
                self.apply_pane_foreground_process_event(
                    pane_id,
                    process_name,
                    process_group_id,
                    current_working_directory,
                )?,
                Some(RenderInvalidationReason::PaneOutput),
            ),
            PaneEvent::InputWritten { pane_id, bytes } => {
                (self.apply_pane_input_written_event(pane_id, bytes)?, None)
            }
            PaneEvent::Output { .. } => {
                return Err(MezError::invalid_state(
                    "pane output must use the output transition path",
                ));
            }
        };
        Ok(self.runtime_transition_with_render(applied, render_reason))
    }

    /// Builds a transition with one render invalidation for every attached client.
    pub(crate) fn runtime_transition_with_render(
        &self,
        applied: bool,
        render_reason: Option<RenderInvalidationReason>,
    ) -> RuntimeTransition {
        let side_effects = if applied {
            render_reason
                .into_iter()
                .flat_map(|reason| {
                    self.session
                        .clients()
                        .iter()
                        .filter(|client| client.state == mez_mux::session::ClientState::Attached)
                        .map(move |client| RuntimeSideEffect::RenderClient {
                            client_id: client.id.clone(),
                            reason,
                        })
                })
                .collect()
        } else {
            Vec::new()
        };
        RuntimeTransition {
            applied,
            side_effects,
        }
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
            .process
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
        if let Some(screen) = self
            .process
            .pane_screens
            .get_mut(descriptor.pane_id.as_str())
        {
            screen.resize(size);
        }
        if let Some(screen) = self
            .process
            .pane_transaction_osc_screens
            .get_mut(descriptor.pane_id.as_str())
        {
            screen.resize(size);
        }
        let primary_pid = self
            .process
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
        self.process
            .pane_current_working_directories
            .remove(process.pane_id.as_str());
        self.fail_agent_turns_for_pane_shutdown(
            std::slice::from_ref(&process.pane_id),
            "pane primary process exited",
        )?;
        self.process.pane_exit_records.insert(
            process.pane_id.clone(),
            PaneExitRecord {
                exit_status: process.status,
            },
        );
        let transition = self
            .session
            .close_exited_pane_with_effects(descriptor.pane_id.as_str())?;
        self.sync_pane_resize_effects(&transition.effects)?;
        if remove_recorded_process {
            self.process
                .pane_processes
                .remove_exited(&process.pane_id)?;
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

    /// Applies pane output through the transport-neutral transition contract.
    pub(crate) fn apply_pane_output_transition(
        &mut self,
        pane_id: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<RuntimeTransition> {
        let update = self.apply_pane_output_bytes(pane_id, bytes)?;
        let applied = update.is_some();
        let render_reason = update.map(|update| {
            if update.invalidate_output_frame {
                RenderInvalidationReason::FullRedraw
            } else {
                RenderInvalidationReason::PaneOutput
            }
        });
        Ok(self.runtime_transition_with_render(applied, render_reason))
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
            .process
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
            &self.process.settings.terminal_term,
        )?;
        let launch =
            mez_mux::process::PaneProcessLaunch::new(self.session.shell.path().to_path_buf());
        let primary_pid = self
            .process
            .pane_processes
            .spawn_for_pane_with_start_directory(
                descriptor.pane_id.as_str(),
                &launch,
                explicit_command,
                &environment,
                descriptor.size,
                start_directory,
            )?;
        self.process
            .pane_exit_records
            .remove(descriptor.pane_id.as_str());
        self.process.pane_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process.pane_transaction_osc_screens.insert(
            descriptor.pane_id.to_string(),
            TerminalScreen::new_with_history_config(
                descriptor.size,
                self.process.settings.terminal_history_limit,
                self.process.settings.terminal_history_rotate_lines,
            )?,
        );
        self.process
            .pane_readiness_states
            .insert(descriptor.pane_id.to_string(), PaneReadinessState::Unknown);
        self.process
            .pane_bootstrap_pending
            .insert(descriptor.pane_id.to_string());
        if let Some(start_directory) = start_directory {
            self.process.pane_current_working_directories.insert(
                descriptor.pane_id.to_string(),
                start_directory.to_path_buf(),
            );
        }

        if self.session.shell.used_fallback() {
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
    /// external pane process adapter.
    ///
    /// The session, screen, readiness, and lifecycle metadata stay in the
    /// runtime service; only PTY/process I/O ownership moves. Callers must start
    /// a replacement external adapter before routing user input away from the
    /// compatibility manager path.
    pub fn take_running_pane_process_for_adapter(&mut self, pane_id: &str) -> Result<PaneProcess> {
        self.require_live()?;
        let primary_pid = self
            .process
            .pane_processes
            .primary_pid(pane_id)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "pane process not found",
                )
            })?;
        if let Some(current_working_directory) = self
            .process
            .pane_processes
            .current_working_directory(pane_id)
        {
            self.process
                .pane_current_working_directories
                .insert(pane_id.to_string(), current_working_directory);
        }
        let process = self
            .process
            .pane_processes
            .take_running_pane_process(pane_id)?;
        self.process
            .detached_pane_primary_pids
            .insert(pane_id.to_string(), primary_pid);
        Ok(process)
    }

    /// Removes up to `limit` running pane processes for pane I/O adapters.
    ///
    /// This is the dynamic production handoff entry point used by the async
    /// pane-process supervisor. Pane state remains in the runtime service while
    /// process, PTY output, input, resize, and termination ownership moves to
    /// one external adapter per returned process.
    pub fn take_running_pane_processes_for_adapter(
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
            .process
            .pane_processes
            .tracked_running_pane_ids()
            .into_iter()
            .take(limit)
            .collect::<Vec<_>>();
        let mut processes = Vec::with_capacity(pane_ids.len());
        for pane_id in pane_ids {
            let process = self.take_running_pane_process_for_adapter(&pane_id)?;
            processes.push((pane_id, process));
        }
        Ok(processes)
    }

    /// Restores a pane process to synchronous manager ownership after a
    /// cancelled external adapter handoff.
    pub fn restore_running_pane_process_from_adapter(
        &mut self,
        pane_id: impl Into<String>,
        process: PaneProcess,
    ) -> Result<u32> {
        self.require_live()?;
        let pane_id = pane_id.into();
        self.process.detached_pane_primary_pids.remove(&pane_id);
        Ok(self
            .process
            .pane_processes
            .insert_running_pane_process(pane_id, process)?)
    }

    /// Drains pane-worker I/O through the transport-neutral transition contract.
    pub(crate) fn drain_pane_io_transition(&mut self) -> RuntimeTransition {
        let side_effects = self.persistence.take_pane_io_effects();
        RuntimeTransition {
            applied: false,
            side_effects,
        }
    }

    /// Returns true when a pane's PTY/process handle is owned by an external adapter.
    pub fn pane_process_is_adapter_owned(&self, pane_id: &str) -> bool {
        self.process
            .detached_pane_primary_pids
            .contains_key(pane_id)
    }

    /// Runs the primary pid for live pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn primary_pid_for_live_pane_process(&self, pane_id: &str) -> Option<u32> {
        self.process
            .pane_processes
            .primary_pid(pane_id)
            .or_else(|| {
                self.process
                    .detached_pane_primary_pids
                    .get(pane_id)
                    .copied()
            })
    }

    /// Writes pane input immediately when the synchronous manager still owns
    /// the pane, or records it for the pane I/O adapter when ownership has
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
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .write_pane_input(pane_id, input)?);
        }
        if self.pane_process_is_adapter_owned(pane_id) {
            self.persistence.queue_pane_input(if priority {
                RuntimeSideEffect::WritePaneInputPriority {
                    pane_id: pane_id.to_string(),
                    bytes: input.to_vec(),
                }
            } else {
                RuntimeSideEffect::WritePaneInput {
                    pane_id: pane_id.to_string(),
                    bytes: input.to_vec(),
                }
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
    /// termination request for an external adapter when ownership has moved.
    pub(super) fn terminate_runtime_pane_process(
        &mut self,
        pane_id: &str,
        force: bool,
    ) -> Result<bool> {
        self.clear_agent_subshell_state(pane_id);
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .terminate_pane(pane_id)
                .map(|process| process.is_some())?);
        }
        if self
            .process
            .detached_pane_primary_pids
            .remove(pane_id)
            .is_some()
        {
            self.persistence.queue_pane_termination(
                pane_id.to_string(),
                RuntimeSideEffect::TerminatePane {
                    pane_id: pane_id.to_string(),
                    force,
                },
            );
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

    /// Terminates all manager-owned and adapter-owned pane processes.
    pub(super) fn terminate_all_runtime_pane_processes(&mut self, force: bool) -> Result<usize> {
        let mut pane_ids = self.process.pane_processes.tracked_pane_ids();
        pane_ids.extend(self.process.detached_pane_primary_pids.keys().cloned());
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
        self.agent_shell_store_mut().remove_session(pane_id);
        self.clear_agent_subshell_state(pane_id);
        self.remove_agent_prompt_input(pane_id);
        self.clear_agent_pane_presentation_preferences(pane_id);
        self.agent_personality_selections.remove(pane_id);
        self.clear_agent_routing_override(pane_id);
        self.clear_agent_pane_artifacts(pane_id);
        self.active_copy_modes_mut().remove(pane_id);
        self.process
            .pane_current_working_directories
            .remove(pane_id);
        self.process.pane_foreground_process_groups.remove(pane_id);
        self.process.program_owned_pane_titles.remove(pane_id);
        self.persistence.cleanup_pane_io(pane_id);
        self.process.pane_screens.remove(pane_id);
        self.process.pane_transaction_osc_screens.remove(pane_id);
        self.process.pane_transaction_osc_pending.remove(pane_id);
        self.process.pane_mez_wrapper_filter_pending.remove(pane_id);
        self.process
            .pane_mez_wrapper_filter_recent_commands
            .remove(pane_id);
        self.process
            .pane_mez_wrapper_filter_recent_polls
            .remove(pane_id);
        self.process
            .pane_hidden_shell_render_recent_polls
            .remove(pane_id);
        self.process.pane_exit_records.remove(pane_id);
        self.process.active_pane_pipes.remove(pane_id);
        self.persistence.remove_pane_transcript_refs(pane_id);
        self.process.pane_readiness_states.remove(pane_id);
        self.process
            .pane_readiness_overrides
            .clear_pending_probe(pane_id);
        self.process
            .pane_readiness_overrides
            .revoke(pane_id, ReadinessOverrideRevocation::PaneClosed);
        self.process.pane_environment_signatures.remove(pane_id);
        self.process.pane_bootstrap_pending.remove(pane_id);
        self.clear_pane_agent_instruction_files(pane_id);
        self.process.pane_closing.remove(pane_id);
        self.clear_terminal_subagent_pane_close(pane_id);
        self.model_profile_overrides.pane_profiles.remove(pane_id);
        self.set_agent_auto_sizing_override(pane_id, None);
        let pane_turn_ids = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| turn.pane_id == pane_id)
            .map(|turn| turn.turn_id.clone())
            .collect::<Vec<_>>();
        for turn_id in &pane_turn_ids {
            self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
            self.remove_subagent_task_parent(turn_id);
            self.clear_joined_subagent_dependencies_for_turn(turn_id);
        }

        let agent_id = format!("agent-{pane_id}");
        self.remove_subagent_task_routes_for_parent(&agent_id);
        self.remove_joined_subagent_dependencies_for_agent(&agent_id);
        self.model_profile_overrides
            .agent_profiles
            .remove(&agent_id);
        self.remove_subagent_authority_state(&agent_id);
        self.deregister_macro_managed_subagent(&agent_id);
        if let Some(agent_id) = AgentId::opaque(agent_id)
            && self
                .message_service
                .registered_identity(&agent_id)
                .is_some()
        {
            let _ = self.message_service.update_presence(
                &agent_id,
                mez_agent::messaging::AgentPresenceStatus::Offline,
                current_unix_seconds().saturating_mul(1000),
            );
        }

        let live_windows = self
            .session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect::<BTreeSet<_>>();
        self.retain_live_subagent_windows(&live_windows);
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
                    if self.process.pane_processes.contains_pane(pane.id.as_str())
                        || self.pane_process_is_adapter_owned(pane.id.as_str())
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
    fn pane_process_size_for(
        &self,
        window: &mez_mux::layout::Window,
        pane_id: &str,
    ) -> Option<Size> {
        let pane = window
            .panes()
            .iter()
            .find(|pane| pane.id.as_str() == pane_id)?;
        let window_frame_visible = self.window_frames_enabled();
        let group_rows = u16::from(self.session.window_groups().len() > 1);
        let display_size = Size::new(
            window.size.columns,
            window.size.rows.saturating_sub(group_rows).max(1),
        )
        .ok()?;
        if window.zoomed_pane_id() == Some(&pane.id) {
            let body_size = rendered_window_body_size(display_size, window_frame_visible);
            let geometry = mez_mux::layout::PaneGeometry {
                index: pane.index,
                column: 0,
                row: 0,
                columns: body_size.columns,
                rows: body_size.rows,
            };
            let content_size = pane_content_size_for_geometry(
                &geometry,
                std::slice::from_ref(&geometry),
                self.pane_frames_enabled(),
                self.pane_frame_position(),
            );
            return Some(
                self.pane_process_size_after_agent_prompt_reservation(
                    pane.id.as_str(),
                    content_size,
                ),
            );
        }

        let body_size = rendered_window_body_size(display_size, window_frame_visible);
        let geometries = window.pane_geometries_for_size(body_size);
        let geometry = geometries
            .iter()
            .find(|geometry| geometry.index == pane.index)?;
        let content_size = pane_content_size_for_geometry(
            geometry,
            &geometries,
            self.pane_frames_enabled(),
            self.pane_frame_position(),
        );
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
