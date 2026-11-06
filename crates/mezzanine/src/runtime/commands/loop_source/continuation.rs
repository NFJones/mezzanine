//! Reuse iterations prepare current history in the existing history lane.
//!
//! The predecessor has already settled. This owner may create only its pinned
//! controller's next iteration, never a new user command or a replayed action.
//! Logical cancellation and physical reader retirement remain independent.

use super::*;
use crate::runtime::{RuntimeAgentCommandLifecyclePhase as Phase, RuntimeAgentLoopContinuation};

impl RuntimeSessionService {
    pub(crate) fn begin_loop_continuation_source(
        &mut self,
        continuation: &RuntimeAgentLoopContinuation,
        predecessor: &str,
    ) -> Result<()> {
        let state = self
            .agent_loop_state_by_id(&continuation.loop_id)
            .filter(|state| {
                state.mode == RuntimeAgentLoopMode::ReuseCurrentConversation
                    && state.iteration == continuation.iteration
                    && state.execution_pane_id == continuation.pane_id
            })
            .cloned()
            .ok_or_else(|| MezError::conflict("loop continuation owner changed"))?;
        if !self
            .agent_turn_ledger()
            .turn(predecessor)
            .is_some_and(|turn| turn.state == AgentTurnState::Completed)
            || !self.agent_loop_turn(predecessor).is_some_and(|turn| {
                turn.loop_id == state.loop_id
                    && turn.iteration.checked_add(1) == Some(state.iteration)
            })
            || self
                .agent_shell_store()
                .get(&continuation.pane_id)
                .is_none_or(|session| session.running_turn_id.is_some())
        {
            return Err(MezError::conflict(
                "loop predecessor has not released its completed turn",
            ));
        }
        let permit = self.reserve_agent_source_preparation()?;
        let work = self.capture_loop_history_work(
            &continuation.pane_id,
            &state.original_prompt,
            state.mode,
            Some(permit),
            Some(predecessor),
        )?;
        let generation = self.begin_agent_command_claim(work.pane(), work.conversation())?;
        self.agent
            .retain_loop_source_work(&continuation.pane_id, generation, work);
        let work = self
            .agent
            .loop_source_work(&continuation.pane_id, generation)
            .ok_or_else(|| MezError::invalid_state("loop continuation source was not retained"))?;
        self.presentation.push_pending_agent_prompt_history(
            crate::runtime::RuntimeAgentHistoryPreparationDispatch::LoopContinuation(work),
        );
        Ok(())
    }

    /// Claims the exact retained allocation. A rejected source fails only the
    /// original controller, and never reads or restores a replacement binding.
    pub(crate) fn claim_loop_continuation_source(&mut self, work: &LoopSourceWork) -> Result<bool> {
        if work.inner.continuation.is_none() {
            return Err(MezError::conflict(
                "history lane received a new-loop command",
            ));
        }
        if !self
            .agent
            .loop_source_work(work.pane(), work.operation)
            .is_some_and(|actual| actual.same_owner(work))
        {
            if self.agent.agent_command_is_cancelled(
                work.pane(),
                work.conversation(),
                work.operation,
            ) {
                self.fail_current_loop_continuation_source(
                    work,
                    MezError::conflict("queued loop source preparation cancelled"),
                )?;
            }
            return Ok(false);
        }
        if let Err(error) = self.validate_loop_source_work(work) {
            self.agent.cancel_matching_agent_command(
                work.pane(),
                work.conversation(),
                work.operation,
            );
            self.fail_current_loop_continuation_source(work, error)?;
            return Ok(false);
        }
        Ok(self
            .agent
            .claim_agent_command(work.pane(), work.conversation(), work.operation))
    }

    /// A late or duplicate result cannot create another iteration. The read is
    /// tied to its original allocation, not merely an equal rendered history.
    pub(crate) fn complete_loop_continuation_source(
        &mut self,
        work: &LoopSourceWork,
        read: Result<LoopSourceRead>,
    ) -> Result<bool> {
        if !self
            .agent
            .loop_source_work(work.pane(), work.operation)
            .is_some_and(|actual| actual.same_owner(work))
            || !self.agent.agent_command_is_claimed(
                work.pane(),
                work.conversation(),
                work.operation,
            )
        {
            if self.agent.agent_command_is_cancelled(
                work.pane(),
                work.conversation(),
                work.operation,
            ) {
                self.fail_current_loop_continuation_source(
                    work,
                    MezError::conflict("loop source preparation cancelled"),
                )?;
            }
            return Ok(false);
        }
        let result = (|| {
            self.validate_loop_source_work(work)?;
            let read = read?;
            if !work.same_owner(&read.owner) || work.inner.continuation.is_none() {
                return Err(MezError::conflict(
                    "loop continuation result has a different source owner",
                ));
            }
            let history = read
                .history
                .ok_or_else(|| MezError::invalid_state("loop continuation history is absent"))?;
            let state =
                work.inner.stale.as_ref().ok_or_else(|| {
                    MezError::invalid_state("loop continuation controller is absent")
                })?;
            let message = format!(
                "loop: continuing fresh iteration {}/{}",
                state.iteration, state.max_iterations
            );
            self.start_agent_loop_work_turn_with_history(work.pane(), Some(history))?;
            // The turn is accepted; optional status rendering cannot roll it
            // back, request another source read or duplicate its scheduling.
            let _ = self.append_agent_status_text_to_terminal_buffer(work.pane(), &message);
            Ok(())
        })();
        self.agent.settle_agent_command(
            work.pane(),
            work.conversation(),
            work.operation,
            if result.is_ok() {
                Phase::Completed
            } else {
                Phase::Failed
            },
        );
        match result {
            Ok(()) => Ok(true),
            Err(error) => {
                self.fail_current_loop_continuation_source(work, error)?;
                Ok(true)
            }
        }
    }

    /// Cancels admission immediately; worker clones retain physical capacity.
    /// Initial source preparation has no controller to terminate.
    pub(crate) fn cancel_pending_loop_source(&mut self, pane: &str) -> Result<()> {
        let work = self.agent.pending_loop_source_work(pane);
        self.agent.cancel_agent_command(pane);
        if let Some(work) = work {
            self.fail_current_loop_continuation_source(
                &work,
                MezError::conflict("loop source preparation cancelled"),
            )?;
        }
        Ok(())
    }

    fn fail_current_loop_continuation_source(
        &mut self,
        work: &LoopSourceWork,
        error: MezError,
    ) -> Result<()> {
        let Some(predecessor) = &work.inner.continuation else {
            return Ok(());
        };
        let Some(original) = &work.inner.stale else {
            return Ok(());
        };
        if self.agent_loop_state_by_id(&original.loop_id) != Some(original)
            || self.pane_has_active_agent_loop_turn(work.pane())
            || matches!(
                self.lifecycle_state(),
                crate::runtime::RuntimeLifecycleState::Stopping
            )
        {
            return Ok(());
        }
        // Reuse restoration is a no-op: an independently replaced conversation
        // must not be rebound to this controller's original source.
        self.recover_failed_agent_loop_continuation(&original.loop_id, predecessor, error)
    }
}
