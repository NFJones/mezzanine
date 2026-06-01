//! Collection-level lifecycle management for pane processes.
//!
//! The manager maps pane identifiers to live process handles and provides the
//! runtime-facing operations for resize, input, output polling, and teardown.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{MezError, Result};
use crate::layout::Size;
use crate::runtime::PaneEnvironment;
use crate::shell::ResolvedShell;

use super::pane::PaneProcess;
use super::spawn::{spawn_pane_process, spawn_pane_process_with_start_directory};
use super::types::{ExitedPaneProcess, PaneExitStatus, PaneProcessOutput};

/// Defines the DEFAULT TERMINATION GRACE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_TERMINATION_GRACE: Duration = Duration::from_millis(500);

/// Carries Pane Process Manager state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
pub struct PaneProcessManager {
    /// Stores the processes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) processes: BTreeMap<String, PaneProcess>,
}

impl PaneProcessManager {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new() -> Self {
        Self {
            processes: BTreeMap::new(),
        }
    }

    /// Runs the spawn for pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn spawn_for_pane(
        &mut self,
        pane_id: impl Into<String>,
        shell: &ResolvedShell,
        explicit_command: Option<&str>,
        environment: &PaneEnvironment,
        size: Size,
    ) -> Result<u32> {
        let pane_id = pane_id.into();
        if self.processes.contains_key(&pane_id) {
            return Err(MezError::conflict("pane already has a primary process"));
        }
        let process = spawn_pane_process(shell, explicit_command, environment, size)?;
        let pid = process.primary_pid();
        self.processes.insert(pane_id, process);
        Ok(pid)
    }

    /// Spawns and tracks a pane process whose shell starts from an explicit directory.
    ///
    /// The pane id must not already be tracked. The start directory is validated
    /// by the spawn layer and is applied to the shell process before any
    /// explicit command is executed.
    pub fn spawn_for_pane_with_start_directory(
        &mut self,
        pane_id: impl Into<String>,
        shell: &ResolvedShell,
        explicit_command: Option<&str>,
        environment: &PaneEnvironment,
        size: Size,
        start_directory: Option<&std::path::Path>,
    ) -> Result<u32> {
        let pane_id = pane_id.into();
        if self.processes.contains_key(&pane_id) {
            return Err(MezError::conflict("pane already has a primary process"));
        }
        let process = spawn_pane_process_with_start_directory(
            shell,
            explicit_command,
            environment,
            size,
            start_directory,
        )?;
        let pid = process.primary_pid();
        self.processes.insert(pane_id, process);
        Ok(pid)
    }

    /// Runs the primary pid operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn primary_pid(&self, pane_id: &str) -> Option<u32> {
        self.processes.get(pane_id).map(PaneProcess::primary_pid)
    }

    /// Returns the live primary process name for a pane.
    ///
    /// The value is sourced from the tracked process handle and is `None` when
    /// the pane is not tracked or the host cannot expose process-name metadata.
    pub fn process_name(&self, pane_id: &str) -> Option<String> {
        self.processes
            .get(pane_id)
            .and_then(PaneProcess::process_name)
    }

    /// Returns the foreground process-group id for a pane's PTY when available.
    pub fn foreground_process_group_id(&self, pane_id: &str) -> Option<u32> {
        self.processes
            .get(pane_id)
            .and_then(PaneProcess::foreground_process_group_id)
    }

    /// Returns the host-reported foreground process name for a pane's PTY.
    pub fn foreground_process_name(&self, pane_id: &str) -> Option<String> {
        self.processes
            .get(pane_id)
            .and_then(PaneProcess::foreground_process_name)
    }

    /// Returns the live primary process current working directory for a pane.
    ///
    /// The value is sourced from the tracked process handle and is `None` when
    /// the pane is not tracked or the host cannot expose process cwd metadata.
    pub fn current_working_directory(&self, pane_id: &str) -> Option<PathBuf> {
        self.processes
            .get(pane_id)
            .and_then(PaneProcess::current_working_directory)
    }

    /// Runs the contains pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn contains_pane(&self, pane_id: &str) -> bool {
        self.processes.contains_key(pane_id)
    }

    /// Returns a monotonic sequence for synchronous pane-output activity waits.
    ///
    /// Callers should read this before polling buffered output, then pass the
    /// sequence to `wait_for_output_activity_after` if no state change was
    /// observed. This avoids races where PTY output arrives between a poll and
    /// a blocking wait.
    pub fn output_activity_sequence(&self, pane_id: &str) -> Option<u64> {
        self.processes
            .get(pane_id)
            .map(PaneProcess::output_activity_sequence)
    }

    /// Blocks until pane-output activity exceeds `sequence` or `timeout` ends.
    ///
    /// Returns `None` when the pane is no longer owned by this process manager.
    pub fn wait_for_output_activity_after(
        &self,
        pane_id: &str,
        sequence: u64,
        timeout: Duration,
    ) -> Option<bool> {
        self.processes
            .get(pane_id)
            .map(|process| process.wait_for_output_activity_after(sequence, timeout))
    }

    /// Removes a live process from manager ownership for async task handoff.
    ///
    /// The process must still be running. Exited processes are retained for the
    /// existing exit-removal path so callers cannot accidentally skip lifecycle
    /// settlement.
    pub fn take_running_pane_process(&mut self, pane_id: &str) -> Result<PaneProcess> {
        let process = self.processes.get(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })?;
        if process.recorded_exit_status().is_some() {
            return Err(MezError::invalid_state(
                "exited pane process cannot be handed to async process owner",
            ));
        }
        self.processes.remove(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })
    }

    /// Inserts a process returned from a cancelled async owner back into the
    /// manager.
    pub fn insert_running_pane_process(
        &mut self,
        pane_id: impl Into<String>,
        process: PaneProcess,
    ) -> Result<u32> {
        let pane_id = pane_id.into();
        if self.processes.contains_key(&pane_id) {
            return Err(MezError::conflict("pane already has a primary process"));
        }
        if process.recorded_exit_status().is_some() {
            return Err(MezError::invalid_state(
                "exited pane process cannot be inserted as running",
            ));
        }
        let pid = process.primary_pid();
        self.processes.insert(pane_id, process);
        Ok(pid)
    }

    /// Runs the tracked pane ids operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn tracked_pane_ids(&self) -> Vec<String> {
        self.processes.keys().cloned().collect()
    }

    /// Returns pane ids whose tracked primary process has not recorded an exit.
    pub fn tracked_running_pane_ids(&self) -> Vec<String> {
        self.processes
            .iter()
            .filter(|(_, process)| process.recorded_exit_status().is_none())
            .map(|(pane_id, _)| pane_id.clone())
            .collect()
    }

    /// Runs the resize pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resize_pane(&self, pane_id: &str, size: Size) -> Result<()> {
        let process = self.processes.get(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })?;
        process.resize(size)
    }

    /// Runs the write pane input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn write_pane_input(&mut self, pane_id: &str, input: &[u8]) -> Result<()> {
        let process = self.processes.get_mut(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })?;
        process.write_input(input)
    }

    /// Runs the read available output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn read_available_output(
        &mut self,
        max_bytes_per_pane: usize,
    ) -> Result<Vec<PaneProcessOutput>> {
        if max_bytes_per_pane == 0 {
            return Err(MezError::invalid_args(
                "pane output read limit must be greater than zero",
            ));
        }
        let mut outputs = Vec::new();
        for pane_id in self.tracked_pane_ids() {
            let Some(process) = self.processes.get_mut(&pane_id) else {
                continue;
            };
            let bytes = process.read_available_output(max_bytes_per_pane)?;
            if !bytes.is_empty() {
                outputs.push(PaneProcessOutput {
                    pane_id,
                    primary_pid: process.primary_pid(),
                    bytes,
                });
            }
        }
        Ok(outputs)
    }

    /// Runs the poll exited operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn poll_exited(&mut self) -> Result<Vec<ExitedPaneProcess>> {
        let mut exited = Vec::new();
        for pane_id in self.tracked_pane_ids() {
            let Some(process) = self.processes.get_mut(&pane_id) else {
                continue;
            };
            if let Some(status) = process.poll_exit()? {
                exited.push(ExitedPaneProcess {
                    pane_id,
                    primary_pid: process.primary_pid(),
                    status,
                });
            }
        }
        Ok(exited)
    }

    /// Runs the remove exited operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn remove_exited(&mut self, pane_id: &str) -> Result<()> {
        let process = self.processes.get(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })?;
        if process.recorded_exit_status().is_none() {
            return Err(MezError::invalid_state(
                "pane process cannot be removed before it exits",
            ));
        }
        self.processes.remove(pane_id);
        Ok(())
    }

    /// Runs the terminate pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminate_pane(&mut self, pane_id: &str) -> Result<Option<ExitedPaneProcess>> {
        self.terminate_pane_with_grace(pane_id, DEFAULT_TERMINATION_GRACE)
    }

    /// Runs the terminate pane with grace operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminate_pane_with_grace(
        &mut self,
        pane_id: &str,
        grace: Duration,
    ) -> Result<Option<ExitedPaneProcess>> {
        let Some(process) = self.processes.get_mut(pane_id) else {
            return Ok(None);
        };
        let primary_pid = process.primary_pid();
        let status = process.terminate(grace)?;
        self.processes.remove(pane_id);
        Ok(Some(ExitedPaneProcess {
            pane_id: pane_id.to_string(),
            primary_pid,
            status,
        }))
    }

    /// Runs the terminate panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminate_panes<'a>(
        &mut self,
        pane_ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<ExitedPaneProcess>> {
        let mut terminated = Vec::new();
        for pane_id in pane_ids {
            if let Some(process) = self.terminate_pane(pane_id)? {
                terminated.push(process);
            }
        }
        Ok(terminated)
    }

    /// Runs the terminate all operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminate_all(&mut self) -> Result<Vec<ExitedPaneProcess>> {
        let pane_ids = self.tracked_pane_ids();
        self.terminate_panes(pane_ids.iter().map(String::as_str))
    }

    /// Runs the wait and remove operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn wait_and_remove(&mut self, pane_id: &str) -> Result<PaneExitStatus> {
        let mut process = self.processes.remove(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane process not found",
            )
        })?;
        process.wait()
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn len(&self) -> usize {
        self.processes.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }
}
