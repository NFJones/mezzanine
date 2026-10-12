//! Terminal-origin status capture and exact-client worker settlement.
//!
//! The shared report owns counters, project and pane incarnation at acceptance.
//! Terminal snapshots use the existing bounded status worker, but never install
//! an agent prompt, mutate its draft, or grant a native conversation authority.
//! Interactive delivery additionally consumes the exact waiting pager generation;
//! textual control delivery is handled by the control report lane instead.

use super::RuntimeStatusReportWork;
use crate::error::{MezError, Result};
use crate::runtime::{
    RuntimeAgentCommandAsyncOutcome, RuntimeAgentCommandAsyncWork, RuntimeAgentCommandDispatch,
    RuntimeAgentCommandLifecyclePhase, RuntimeAgentShellDisplayOutput, RuntimeSessionService,
};

impl RuntimeSessionService {
    /// Captures a terminal snapshot before queueing any off-actor report work.
    pub(crate) fn dispatch_terminal_status_report(
        &mut self,
        client: &mez_core::ids::ClientId,
        pane: &str,
        options: mez_agent::slash::StatusOptions,
    ) -> Result<()> {
        let visibility = self
            .agent_shell_store()
            .get(pane)
            .map(|session| session.visibility);
        let report = self
            .prepare_status_report(client, pane, options)?
            .for_terminal(visibility);
        let conversation_id = report.command_owner();
        let claim_generation = self.begin_agent_command_claim(pane, &conversation_id)?;
        self.retain_pending_status_report(pane, claim_generation, report);
        self.presentation
            .push_pending_deferred_agent_command(RuntimeAgentCommandDispatch {
                primary_client_id: client.clone(),
                pane_id: pane.into(),
                conversation_id,
                command: "agent-status".into(),
                input: "agent-status".into(),
                claim_generation,
            });
        Ok(())
    }

    /// Installs only a current terminal pager, never an agent display state.
    pub(crate) fn complete_terminal_status_report(
        &mut self,
        work: &RuntimeAgentCommandAsyncWork,
        report: &RuntimeStatusReportWork,
        outcome: RuntimeAgentCommandAsyncOutcome,
    ) -> Result<bool> {
        let current = report
            .terminal_overlay_generation
            .is_some_and(|generation| {
                self.presentation
                    .terminal_status_overlay_is_current(generation)
            })
            && self.agent.agent_command_is_claimed(
                &work.pane_id,
                &work.conversation_id,
                work.claim_generation,
            );
        if !current {
            self.agent.cancel_matching_agent_command(
                &work.pane_id,
                &work.conversation_id,
                work.claim_generation,
            );
            return Ok(false);
        }
        let projected = Self::project_deferred_agent_command_outcome(work, outcome)?;
        let RuntimeAgentCommandAsyncOutcome::Projected {
            display_output,
            failed,
            ..
        } = projected
        else {
            return Err(MezError::invalid_state(
                "terminal status projection unavailable",
            ));
        };
        match display_output {
            RuntimeAgentShellDisplayOutput::Overlay(content) => {
                self.present_runtime_command_display_content(content)?
            }
            RuntimeAgentShellDisplayOutput::TransientErrorStatus(lines)
            | RuntimeAgentShellDisplayOutput::Lines(lines) => {
                self.show_primary_error_overlay(lines)?
            }
            _ => {
                return Err(MezError::invalid_state(
                    "terminal status display unavailable",
                ));
            }
        }
        self.presentation.capture_projected_client_state();
        Ok(self.agent.settle_agent_command(
            &work.pane_id,
            &work.conversation_id,
            work.claim_generation,
            if failed {
                RuntimeAgentCommandLifecyclePhase::Failed
            } else {
                RuntimeAgentCommandLifecyclePhase::Completed
            },
        ))
    }
}
