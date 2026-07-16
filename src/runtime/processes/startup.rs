//! Runtime process helpers for initial startup and snapshot restoration.
//!
//! This module owns initial pane startup, restored-pane restart, snapshot
//! terminal-screen seeding, and snapshot-resume hook checks. The parent
//! processes module keeps live polling, output handling, transactions, and
//! low-level pane I/O while this child module keeps restored-process startup
//! and snapshot seeding rules together.

use super::*;

/// Maximum time spent waiting for freshly restored shells to print their first
/// prompt bytes during a synchronous layout load.
const RESTORED_PANE_INITIAL_OUTPUT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(150);

/// Returns the user's home directory when it is available and usable as a
/// pane process start directory.
fn runtime_home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .filter(|home| home.is_dir())
}

impl RuntimeSessionService {
    /// Runs the start initial pane process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_initial_pane_process(
        &mut self,
        explicit_command: Option<&str>,
    ) -> Result<PaneProcessStart> {
        self.require_live()?;
        let descriptor = self.initial_pane_descriptor()?;
        let started = self.start_pane_process(descriptor, explicit_command)?;
        self.run_configured_completed_hooks(
            HookEvent::SessionStart,
            &format!(
                r#"{{"session_id":"{}","initial_pane_id":"{}"}}"#,
                json_escape(self.session.id.as_str()),
                json_escape(&started.pane_id)
            ),
        )?;
        Ok(started)
    }

    /// Runs the restart restored pane processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn restart_restored_pane_processes(
        &mut self,
        explicit_command: Option<&str>,
    ) -> Result<Vec<PaneProcessStart>> {
        self.require_live()?;
        let descriptors = self
            .session
            .windows()
            .iter()
            .flat_map(|window| {
                window.panes().iter().filter_map(|pane| {
                    if pane.live || self.process.pane_processes.contains_pane(pane.id.as_str()) {
                        None
                    } else {
                        let size = self
                            .pane_process_size_for(window, pane.id.as_str())
                            .unwrap_or(pane.size);
                        Some(PaneDescriptor {
                            window_id: window.id.clone(),
                            pane_id: pane.id.clone(),
                            size,
                        })
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut starts = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            let restored_screen = self
                .process
                .pane_screens
                .get(descriptor.pane_id.as_str())
                .cloned();
            let start_directory = self.restored_pane_start_directory(descriptor.pane_id.as_str());
            let started = self.start_restored_pane_process_with_best_effort_directory(
                descriptor,
                explicit_command,
                start_directory.as_deref(),
            )?;
            if let Some(mut screen) = restored_screen {
                screen.feed(b"\n[mezzanine: pane restarted with a fresh primary PID]\n");
                self.process
                    .pane_screens
                    .insert(started.pane_id.clone(), screen);
            }
            self.set_pane_readiness(&started.pane_id, PaneReadinessState::PromptCandidate);
            self.session.set_pane_live_state(&started.pane_id, true)?;
            self.append_lifecycle_event(
                EventKind::PaneChanged,
                format!(
                    r#"{{"pane_id":"{}","window_id":"{}","primary_pid":{},"process_state":"running","restarted":true}}"#,
                    json_escape(&started.pane_id),
                    json_escape(&started.window_id),
                    started.primary_pid
                ),
            )?;
            starts.push(started);
        }
        self.drain_restored_pane_initial_output(&starts)?;
        Ok(starts)
    }

    /// Waits briefly for newly restored panes to emit their first visible bytes.
    ///
    /// A `load-layout` command immediately redraws the attached client after it
    /// restarts pane shells. Without a bounded drain here, that redraw can race
    /// the shell's first prompt repaint and leave panes apparently blank until
    /// the user's next keypress causes another output poll.
    fn drain_restored_pane_initial_output(&mut self, starts: &[PaneProcessStart]) -> Result<()> {
        if starts.is_empty() {
            return Ok(());
        }
        let mut pending = starts
            .iter()
            .filter_map(|start| {
                self.process
                    .pane_processes
                    .output_activity_sequence(start.pane_id.as_str())
                    .map(|sequence| (start.pane_id.clone(), sequence))
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        self.poll_pane_outputs(crate::runtime::DEFAULT_PTY_READ_LIMIT_BYTES)?;
        pending.retain(|(pane_id, sequence)| {
            matches!(
                self.process.pane_processes.output_activity_sequence(pane_id.as_str()),
                Some(current) if current <= *sequence
            )
        });
        if pending.is_empty() {
            return Ok(());
        }
        let deadline = std::time::Instant::now() + RESTORED_PANE_INITIAL_OUTPUT_WAIT;
        let wait_slice = std::time::Duration::from_millis(10);
        while !pending.is_empty() {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let mut index = 0usize;
            let slice = remaining.min(wait_slice);
            while index < pending.len() {
                let pane_id = pending[index].0.clone();
                let sequence = pending[index].1;
                if self
                    .process
                    .pane_processes
                    .wait_for_output_activity_after(pane_id.as_str(), sequence, slice)
                    .unwrap_or(false)
                {
                    self.poll_pane_outputs(crate::runtime::DEFAULT_PTY_READ_LIMIT_BYTES)?;
                    pending.retain(|(pending_pane_id, pending_sequence)| {
                        matches!(
                            self.process.pane_processes
                                .output_activity_sequence(pending_pane_id.as_str()),
                            Some(current) if current <= *pending_sequence
                        )
                    });
                } else {
                    index = index.saturating_add(1);
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
            }
        }
        self.poll_pane_outputs(crate::runtime::DEFAULT_PTY_READ_LIMIT_BYTES)?;
        Ok(())
    }

    /// Starts one restored pane while treating its snapshot working directory
    /// as best-effort state rather than a resume-critical invariant.
    fn start_restored_pane_process_with_best_effort_directory(
        &mut self,
        descriptor: PaneDescriptor,
        explicit_command: Option<&str>,
        start_directory: Option<&Path>,
    ) -> Result<PaneProcessStart> {
        match self.start_pane_process_with_start_directory(
            descriptor.clone(),
            explicit_command,
            start_directory,
        ) {
            Ok(started) => Ok(started),
            Err(error) if start_directory.is_some() => {
                let home_directory = runtime_home_directory();
                self.append_lifecycle_event(
                    EventKind::Diagnostic,
                    format!(
                        r#"{{"pane_id":"{}","diagnostic":"snapshot resume pane cwd unavailable; retrying from home","error":"{}"}}"#,
                        json_escape(descriptor.pane_id.as_str()),
                        json_escape(&error.to_string())
                    ),
                )?;
                self.start_pane_process_with_start_directory(
                    descriptor,
                    explicit_command,
                    home_directory.as_deref(),
                )
            }
            Err(error) => Err(error),
        }
    }

    /// Returns the start directory for a restored pane's fresh shell.
    fn restored_pane_start_directory(&self, pane_id: &str) -> Option<PathBuf> {
        self.session
            .pane_state_metadata(pane_id)
            .and_then(|metadata| metadata.current_working_directory.as_deref())
            .map(PathBuf::from)
            .filter(|directory| directory.is_dir())
            .or_else(runtime_home_directory)
    }

    /// Runs the seed terminal screens from snapshot payload operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn seed_terminal_screens_from_snapshot_payload(
        &mut self,
        payload: &SessionSnapshotPayload,
    ) -> Result<usize> {
        self.require_live()?;
        self.require_snapshot_resume_hooks_allow(payload)?;
        self.seed_terminal_screens_from_snapshot_payload_without_hooks(payload)
    }

    /// Runs the require snapshot resume hooks allow operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn require_snapshot_resume_hooks_allow(
        &mut self,
        payload: &SessionSnapshotPayload,
    ) -> Result<()> {
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::LayoutLoad,
            &format!(
                r#"{{"session_id":"{}","windows":{},"panes":{}}}"#,
                json_escape(&payload.session_id),
                payload.windows.len(),
                payload
                    .windows
                    .iter()
                    .map(|window| window.panes.len())
                    .sum::<usize>()
            ),
        )? {
            return Err(MezError::forbidden(format!(
                "snapshot resume blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        Ok(())
    }

    /// Runs the seed terminal screens from snapshot payload without hooks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn seed_terminal_screens_from_snapshot_payload_without_hooks(
        &mut self,
        payload: &SessionSnapshotPayload,
    ) -> Result<usize> {
        let mut seeded = 0usize;
        for window in &payload.windows {
            for pane in &window.panes {
                let Some(descriptor) = self.find_pane_descriptor(&pane.pane_id) else {
                    continue;
                };
                let mut screen = TerminalScreen::new_with_history_config(
                    descriptor.size,
                    self.terminal_history_limit,
                    self.terminal_history_rotate_lines,
                )?;
                let history_lines = pane
                    .terminal_history
                    .iter()
                    .enumerate()
                    .map(|(line_index, line)| TerminalStyledLine {
                        text: line.clone(),
                        style_spans: pane
                            .terminal_history_line_style_spans
                            .get(line_index)
                            .cloned()
                            .unwrap_or_default(),
                        copy_text: None,
                    })
                    .collect::<Vec<_>>();
                let visible_lines = pane
                    .visible_lines
                    .iter()
                    .enumerate()
                    .map(|(line_index, line)| TerminalStyledLine {
                        text: line.clone(),
                        style_spans: pane
                            .visible_line_style_spans
                            .get(line_index)
                            .cloned()
                            .unwrap_or_default(),
                        copy_text: None,
                    })
                    .collect::<Vec<_>>();
                screen.restore_normal_styled_history_content(&history_lines, &visible_lines);
                screen.restore_mode_state(&pane.terminal_modes);
                screen.restore_saved_state(&pane.terminal_saved_state);
                self.process
                    .pane_screens
                    .insert(pane.pane_id.clone(), screen);
                seeded = seeded.saturating_add(1);
            }
        }
        if seeded > 0 {
            self.append_lifecycle_event(
                EventKind::SnapshotChanged,
                format!(r#"{{"snapshot_restore":"terminal_screens_seeded","panes":{seeded}}}"#),
            )?;
        }
        Ok(seeded)
    }
}
