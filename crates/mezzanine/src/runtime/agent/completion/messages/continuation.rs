//! Bounded original scheduler wakes after canonical message-result completion.
//!
//! Storage does not hold a provider slot. A parked original returns through the
//! existing ready queue; QueueFull retains only its actual group identity and a
//! bounded wake. Repeated observation cannot re-send, reapprove or insert work for
//! a replacement group. Other live action/dependency owners keep their own wakes.

use super::*;

impl RuntimeSessionService {
    /// Retains one exact group wake after its message results/context are owned.
    pub(in crate::runtime::agent) fn retain_stored_message_continuation(
        &mut self,
        turn_id: &str,
    ) -> Result<()> {
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("message continuation group unavailable"))?;
        self.agent
            .completion_settlements
            .message_continuations
            .insert(turn_id.to_owned(), Arc::clone(&group.identity));
        Ok(())
    }

    /// Enables a bounded actor timer only for eligible, unpaused original wakes.
    pub(crate) fn stored_message_continuation_needs_wake(&self) -> bool {
        !self.agent.completion_settlements.message_storage_stopping
            && self
                .agent
                .completion_settlements
                .message_continuations
                .keys()
                .any(|id| !self.agent_turn_is_human_paused(id))
    }

    /// Re-admits completed original sources through the existing fair scheduler.
    /// QueueFull remains owned; stale/retired groups lose only this wake metadata.
    pub(crate) fn recover_stored_message_continuations(&mut self) -> Result<()> {
        if self.agent.completion_settlements.message_storage_stopping {
            return Ok(());
        }
        let wakes = self
            .agent
            .completion_settlements
            .message_continuations
            .iter()
            .map(|(id, identity)| (id.clone(), Arc::clone(identity)))
            .collect::<Vec<_>>();
        for (id, identity) in wakes {
            let current = self
                .agent
                .completion_settlements
                .groups
                .get(&id)
                .is_some_and(|group| {
                    Arc::ptr_eq(&group.identity, &identity)
                        && !group.paint_revoked
                        && group.primary_failure.is_none()
                });
            if !current {
                self.agent
                    .completion_settlements
                    .message_continuations
                    .remove(&id);
                continue;
            }
            if self.agent_turn_is_human_paused(&id) || self.message_completion_pending(&id) {
                continue;
            }
            let Some(execution) = self.agent_turn_executions().get(&id).cloned() else {
                self.agent
                    .completion_settlements
                    .message_continuations
                    .remove(&id);
                continue;
            };
            if execution.terminal_state == AgentTurnState::Running
                && !self.completion_provider_continuation_ready(&id)
            {
                // A real wait, join or asynchronous sibling still owns readiness.
                self.agent
                    .completion_settlements
                    .message_continuations
                    .remove(&id);
                continue;
            }
            let waiting = self
                .agent
                .agent_scheduler
                .waiting_turns()
                .any(|work| work.turn_id == id);
            let blocked = self
                .agent
                .agent_scheduler
                .blocked_turns()
                .any(|work| work.turn_id == id);
            if waiting || blocked {
                let queued = if waiting {
                    self.agent.agent_scheduler.requeue_waiting(&id)
                } else {
                    self.agent.agent_scheduler.requeue_blocked(&id)
                };
                match queued {
                    Ok(_) => {}
                    Err(error) if error.kind() == mez_agent::SchedulerErrorKind::QueueFull => {
                        continue;
                    }
                    Err(error) => {
                        return Err(self.retain_completion_settlement_error(
                            &id,
                            "message_scheduler_wake",
                            error.into(),
                        ));
                    }
                }
                if let Err(error) = self.start_ready_agent_turns() {
                    return Err(self.retain_completion_settlement_error(
                        &id,
                        "message_scheduler_ready",
                        error,
                    ));
                }
            }
            let running = self
                .agent_turn_ledger()
                .turn(&id)
                .is_some_and(|turn| turn.state == AgentTurnState::Running);
            if running {
                self.recover_retained_completion_settlement(&id)?;
            }
            // A successful requeue has its existing scheduler owner even when
            // another runnable turn retains the current permit. Publication-only
            // recovery likewise stays under the common group settlement intent.
            self.agent
                .completion_settlements
                .message_continuations
                .remove(&id);
        }
        Ok(())
    }
}
