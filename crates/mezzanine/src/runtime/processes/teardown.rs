//! Pane process termination through the current runtime ownership boundary.
//!
//! Subshell authority is cleared before termination. Adapter effects retain the
//! exact process generation and force flag; manager-owned processes are settled
//! synchronously without introducing another lifecycle state owner.

use super::*;

impl RuntimeSessionService {
    /// Terminates a pane process immediately when manager-owned, or queues a
    /// termination request for an external adapter when ownership has moved.
    pub(in crate::runtime) fn terminate_runtime_pane_process(
        &mut self,
        pane_id: &str,
        force: bool,
    ) -> Result<bool> {
        self.clear_agent_subshell_state(pane_id);
        self.clear_agent_subshell_shell_identity(pane_id);
        if self.process.pane_processes.contains_pane(pane_id) {
            return Ok(self
                .process
                .pane_processes
                .terminate_pane(pane_id)
                .map(|process| process.is_some())?);
        }
        if let Some(process) = self.process.detached_pane_processes.get(pane_id).copied() {
            self.persistence.queue_pane_termination(
                pane_id.to_string(),
                RuntimeSideEffect::PaneProcessIo {
                    instance: PaneProcessInstance {
                        pane_id: pane_id.to_string(),
                        generation: process.generation,
                    },
                    effect: PaneProcessIoEffect::Terminate { force },
                },
            );
            return Ok(true);
        }
        Ok(false)
    }

    /// Terminates each listed pane process through the current owner boundary.
    pub(in crate::runtime) fn terminate_runtime_pane_processes<'a>(
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
    pub(in crate::runtime) fn terminate_all_runtime_pane_processes(
        &mut self,
        force: bool,
    ) -> Result<usize> {
        let mut pane_ids = self.process.pane_processes.tracked_pane_ids();
        pane_ids.extend(self.process.detached_pane_processes.keys().cloned());
        self.terminate_runtime_pane_processes(pane_ids.iter().map(String::as_str), force)
    }
}
