//! Runtime active pane-pipe lifecycle management.
//!
//! This module owns live `pipe-pane` output routing, deferred file pipe
//! writes, command-backed pipe health checks, and user-visible pane-pipe
//! lifecycle events. Keeping these helpers together keeps the process facade
//! focused on pane process lifecycle orchestration.

use super::{
    ActivePanePipe, EventKind, MezError, Path, PathBuf, Result, RuntimeSessionService,
    StoppedPanePipe, json_escape,
};
use crate::runtime::{
    PersistenceTarget, PersistenceWriteMode, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
    RuntimeTransition,
};

impl RuntimeSessionService {
    /// Runs the write active pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn write_active_pane_pipe(&mut self, pane_id: &str, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let use_external_effect_adapter = self.persistence.pane_pipe_uses_adapter();
        let Some(pipe) = self.process.active_pane_pipes.get_mut(pane_id) else {
            return Ok(());
        };
        if use_external_effect_adapter && let Some(path) = pipe.file_target_path() {
            pipe.record_deferred_output(bytes.len());
            self.persistence.queue_pane_pipe(
                pane_id.to_string(),
                RuntimeSideEffect::Persist {
                    target: PersistenceTarget::PanePipe,
                    path,
                    bytes: bytes.to_vec(),
                    mode: PersistenceWriteMode::Append,
                },
            );
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
    pub(crate) fn start_file_pane_pipe(
        &mut self,
        pane_id: String,
        path: PathBuf,
    ) -> Result<String> {
        let _ = self.stop_active_pane_pipe(pane_id.as_str());
        let pipe = if self.persistence.pane_pipe_uses_adapter() {
            ActivePanePipe::deferred_file(pane_id.clone(), path)
        } else {
            ActivePanePipe::file(pane_id.clone(), path)?
        };
        let body = format!(
            "target={}:pipe=started:mode={}:output={}:active_pipes={}",
            pipe.pane_id,
            pipe.mode(),
            pipe.target_label(),
            self.process.active_pane_pipes.len().saturating_add(1)
        );
        self.process.active_pane_pipes.insert(pane_id, pipe);
        Ok(body)
    }

    /// Runs the start command pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn start_command_pane_pipe(
        &mut self,
        pane_id: String,
        command: String,
    ) -> Result<String> {
        let _ = self.stop_active_pane_pipe(pane_id.as_str());
        let pipe = if self.persistence.pane_pipe_uses_adapter() {
            ActivePanePipe::deferred_command(pane_id.clone(), self.session.shell.path(), command)?
        } else {
            ActivePanePipe::command(pane_id.clone(), self.session.shell.path(), command)?
        };
        let body = format!(
            "target={}:pipe=started:mode={}:command={}:active_pipes={}",
            pipe.pane_id,
            pipe.mode(),
            pipe.target_label(),
            self.process.active_pane_pipes.len().saturating_add(1)
        );
        self.process.active_pane_pipes.insert(pane_id, pipe);
        Ok(body)
    }

    /// Runs the stop active pane pipe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn stop_active_pane_pipe(&mut self, pane_id: &str) -> Result<StoppedPanePipe> {
        let pipe = self
            .process
            .active_pane_pipes
            .remove(pane_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "pane pipe not found")
            })?;
        Ok(pipe.stop())
    }

    /// Runs the stop active pane pipes for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn stop_active_pane_pipes_for(&mut self, pane_ids: &[&str]) -> Vec<StoppedPanePipe> {
        pane_ids
            .iter()
            .filter_map(|pane_id| {
                self.process
                    .active_pane_pipes
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
    pub(crate) fn stop_all_active_pane_pipes(&mut self) -> Vec<StoppedPanePipe> {
        std::mem::take(&mut self.process.active_pane_pipes)
            .into_values()
            .map(ActivePanePipe::stop)
            .collect()
    }

    /// Returns whether the pane has a command-backed pipe that should be
    /// checked by the actor-owned health timer after accepted output.
    pub(crate) fn command_pane_pipe_health_check_needed(&self, pane_id: &str) -> Result<bool> {
        self.process
            .active_pane_pipes
            .get(pane_id)
            .map(|pipe| pipe.command_status())
            .transpose()
            .map(|status| status.flatten().is_some())
    }

    /// Builds the desired command-pane-pipe health timer transition.
    pub(crate) fn pane_pipe_health_timer_transition(
        &self,
        pane_id: &str,
        active_key: Option<RuntimeTimerKey>,
        next_generation: u64,
        delay_ms: u64,
    ) -> Result<RuntimeTransition> {
        if !self.command_pane_pipe_health_check_needed(pane_id)? {
            return Ok(RuntimeTransition {
                applied: false,
                side_effects: active_key
                    .map(|key| RuntimeSideEffect::CancelTimer { key })
                    .into_iter()
                    .collect(),
            });
        }
        if active_key.is_some() {
            return Ok(RuntimeTransition::default());
        }
        Ok(RuntimeTransition {
            applied: false,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::PanePipeHealth,
                    pane_id,
                    next_generation,
                ),
                delay_ms,
            }],
        })
    }

    /// Returns pane ids that currently have command-backed pipes.
    pub(crate) fn active_command_pane_pipe_ids(&self) -> Vec<String> {
        self.process
            .active_pane_pipes
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
            .process
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
            .process
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

    /// Drains file-backed pane-pipe writes through the runtime transition contract.
    pub(crate) fn drain_pane_pipe_persistence_transition(&mut self) -> RuntimeTransition {
        RuntimeTransition {
            applied: false,
            side_effects: self.persistence.take_pane_pipe_effects(),
        }
    }

    /// Returns the user-facing active pane pipe status line used by
    /// `pipe-pane` and async actor tests that verify pipe lifecycle state.
    pub(crate) fn active_pane_pipe_display(&self) -> String {
        if self.process.active_pane_pipes.is_empty() {
            return "active_pipes=0".to_string();
        }
        self.process
            .active_pane_pipes
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
}
