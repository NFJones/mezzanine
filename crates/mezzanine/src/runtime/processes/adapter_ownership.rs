//! Exact PTY/process ownership handoff between runtime manager and async adapters.
//!
//! Only process I/O ownership moves. Session, readiness, screen, and lifecycle
//! authority remain in RuntimeSessionService; generation checks reject stale
//! adapter events after replacement at a reused pane identity.

use super::*;

impl RuntimeSessionService {
    /// Transfers one running PTY to an adapter, retaining runtime metadata.
    /// The caller must install the adapter before routing deferred input.
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
        self.process.next_detached_pane_generation = self
            .process
            .next_detached_pane_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("pane process generation exhausted"))?;
        self.process.detached_pane_processes.insert(
            pane_id.to_string(),
            DetachedPaneProcess {
                primary_pid,
                generation: self.process.next_detached_pane_generation,
            },
        );
        Ok(process)
    }

    /// Transfers up to a positive limit of running processes with exact identities.
    /// Pending editor processes retain precedence over ordinary pane processes.
    pub fn take_running_pane_process_instances_for_adapter(
        &mut self,
        limit: usize,
    ) -> Result<Vec<(PaneProcessInstance, PaneProcess)>> {
        self.require_live()?;
        if limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process handoff limit must be greater than zero",
            ));
        }
        let mut processes = self.take_pending_external_editor_processes(limit);
        let remaining = limit.saturating_sub(processes.len());
        let pane_ids = self
            .process
            .pane_processes
            .tracked_running_pane_ids()
            .into_iter()
            .take(remaining)
            .collect::<Vec<_>>();
        for pane_id in pane_ids {
            let process = self.take_running_pane_process_for_adapter(&pane_id)?;
            let generation = self
                .process
                .detached_pane_processes
                .get(&pane_id)
                .map(|detached| detached.generation)
                .ok_or_else(|| {
                    MezError::invalid_state("adapter-owned pane process identity was not recorded")
                })?;
            processes.push((
                PaneProcessInstance {
                    pane_id,
                    generation,
                },
                process,
            ));
        }
        Ok(processes)
    }

    /// Retains the pane-id-only handoff shape for synchronous test fixtures.
    #[cfg(test)]
    pub fn take_running_pane_processes_for_adapter(
        &mut self,
        limit: usize,
    ) -> Result<Vec<(String, PaneProcess)>> {
        self.take_running_pane_process_instances_for_adapter(limit)
            .map(|processes| {
                processes
                    .into_iter()
                    .map(|(instance, process)| (instance.pane_id, process))
                    .collect()
            })
    }

    /// Restores manager ownership after a cancelled adapter handoff in fixtures.
    #[cfg(test)]
    pub fn restore_running_pane_process_from_adapter(
        &mut self,
        pane_id: impl Into<String>,
        process: PaneProcess,
    ) -> Result<u32> {
        self.require_live()?;
        let pane_id = pane_id.into();
        self.process.detached_pane_processes.remove(&pane_id);
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

    /// Reports whether an external adapter owns the pane's PTY/process handle.
    pub fn pane_process_is_adapter_owned(&self, pane_id: &str) -> bool {
        self.process.detached_pane_processes.contains_key(pane_id)
    }

    /// Checks exact editor or live-pane generation before accepting adapter work.
    pub(crate) fn pane_process_instance_is_current(&self, instance: &PaneProcessInstance) -> bool {
        self.external_editor_process_instance_is_current(instance)
            || (self.find_pane_descriptor(&instance.pane_id).is_some()
                && self
                    .process
                    .detached_pane_processes
                    .get(&instance.pane_id)
                    .is_some_and(|process| process.generation == instance.generation))
    }

    /// Returns the current adapter-owned process identity for one pane.
    pub(crate) fn adapter_owned_pane_process_instance(
        &self,
        pane_id: &str,
    ) -> Option<PaneProcessInstance> {
        self.process
            .detached_pane_processes
            .get(pane_id)
            .map(|process| PaneProcessInstance {
                pane_id: pane_id.to_string(),
                generation: process.generation,
            })
    }

    /// Returns the root PID from either current process owner, when present.
    pub(in crate::runtime) fn primary_pid_for_live_pane_process(
        &self,
        pane_id: &str,
    ) -> Option<u32> {
        self.process
            .pane_processes
            .primary_pid(pane_id)
            .or_else(|| {
                self.process
                    .detached_pane_processes
                    .get(pane_id)
                    .map(|process| process.primary_pid)
            })
    }
}
