//! Foreground-process and terminal-owned pane title synchronization.

use super::super::*;
use crate::runtime::service_state::ProgramOwnedPaneTitle;

/// Number of idle polls between foreground-process title refreshes.
const RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL: usize = 16;

impl RuntimeSessionService {
    /// Runs the sync pane titles from foreground processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime::processes) fn sync_pane_titles_from_foreground_processes(
        &mut self,
        skipped_panes: &BTreeSet<String>,
    ) -> Result<usize> {
        let mut changed = 0usize;
        for pane_id in self.pane_processes.tracked_pane_ids() {
            if skipped_panes.contains(&pane_id) {
                continue;
            }
            let Some((title, foreground_group)) = self.foreground_process_pane_title(&pane_id)
            else {
                continue;
            };
            let mut title_changed =
                self.restore_program_pane_title_for_foreground_change(&pane_id, foreground_group)?;
            if self
                .process
                .program_owned_pane_titles
                .contains_key(&pane_id)
            {
                continue;
            }
            title_changed |= self
                .session
                .set_pane_title_from_terminal(pane_id.as_str(), title)?;
            if !title_changed {
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
    pub(in crate::runtime::processes) fn should_sync_pane_titles_from_foreground_processes(
        &mut self,
        observed_output: bool,
    ) -> bool {
        if observed_output {
            self.process.foreground_title_idle_sync_polls = 0;
            return true;
        }
        let should_sync = self.process.foreground_title_idle_sync_polls == 0;
        self.process.foreground_title_idle_sync_polls = self
            .process
            .foreground_title_idle_sync_polls
            .saturating_add(1)
            % RUNTIME_FOREGROUND_TITLE_IDLE_SYNC_POLL_INTERVAL;
        should_sync
    }

    /// Runs the foreground process pane title operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn foreground_process_pane_title(&self, pane_id: &str) -> Option<(String, u32)> {
        let foreground_name = self.pane_processes.foreground_process_name(pane_id)?;
        let foreground_group = self.pane_processes.foreground_process_group_id(pane_id)?;
        let primary_pid = self.pane_processes.primary_pid(pane_id)?;
        self.title_from_foreground_process_metadata(
            pane_id,
            foreground_name,
            foreground_group,
            primary_pid,
        )
        .map(|title| (title, foreground_group))
    }

    /// Sets a pane title emitted by a foreground program and captures the prior mode.
    pub(super) fn set_pane_title_from_program_output(
        &mut self,
        pane_id: &str,
        title: String,
        foreground_process_group_id: u32,
    ) -> Result<bool> {
        if self
            .process
            .program_owned_pane_titles
            .get(pane_id)
            .is_some_and(|state| state.foreground_process_group_id != foreground_process_group_id)
        {
            let _ = self.restore_program_pane_title_for_foreground_change(
                pane_id,
                foreground_process_group_id,
            )?;
        }
        if !self.process.program_owned_pane_titles.contains_key(pane_id) {
            let (previous_title, previous_source) = self.session.pane_title_state(pane_id)?;
            if previous_source.is_explicit() {
                return Ok(false);
            }
            self.process.program_owned_pane_titles.insert(
                pane_id.to_string(),
                ProgramOwnedPaneTitle {
                    foreground_process_group_id,
                    previous_title,
                    previous_source,
                },
            );
        }
        Ok(self.session.set_pane_title_from_program(pane_id, title)?)
    }

    /// Restores the saved title mode when a program-title owner is no longer foreground.
    fn restore_program_pane_title_for_foreground_change(
        &mut self,
        pane_id: &str,
        foreground_process_group_id: u32,
    ) -> Result<bool> {
        if self
            .process
            .program_owned_pane_titles
            .get(pane_id)
            .is_some_and(|state| state.foreground_process_group_id == foreground_process_group_id)
        {
            return Ok(false);
        }
        let Some(state) = self.process.program_owned_pane_titles.remove(pane_id) else {
            return Ok(false);
        };
        Ok(self.session.restore_pane_title_state(
            pane_id,
            state.previous_title,
            state.previous_source,
        )?)
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
        self.process
            .pane_foreground_process_groups
            .insert(pane_id.clone(), process_group_id);
        if self.pane_foreground_primary_shell_state(&pane_id) == Some(true) {
            let _ = self.observe_passive_shell_prompt_candidate(
                pane_id.as_str(),
                "foreground-process-event",
            )?;
        }
        let Some(title) = self.title_from_foreground_process_metadata(
            &pane_id,
            process_name.into(),
            process_group_id,
            primary_pid,
        ) else {
            return Ok(false);
        };
        let mut title_changed =
            self.restore_program_pane_title_for_foreground_change(&pane_id, process_group_id)?;
        if self
            .process
            .program_owned_pane_titles
            .contains_key(&pane_id)
        {
            return Ok(false);
        }
        title_changed |= self
            .session
            .set_pane_title_from_terminal(pane_id.as_str(), title)?;
        if !title_changed {
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
    pub(in crate::runtime) fn pane_current_working_directory(
        &self,
        pane_id: &str,
    ) -> Option<PathBuf> {
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
        _pane_id: &str,
        foreground_name: String,
        foreground_group: u32,
        primary_pid: u32,
    ) -> Option<String> {
        if foreground_group == primary_pid
            && Some(foreground_name.as_str()) == self.session.shell.path().file_name()?.to_str()
        {
            return Some("shell".to_string());
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
            invalidate_output_frame: false,
        })
    }
}
