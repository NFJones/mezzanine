//! Actor-owned human dispatch inhibition, distinct from interruption and waits.
//!
//! Issued work remains owned until settlement. Only explicit attached-primary
//! resume removes this gate; mail and approval/dependency changes cannot do so.
//! Runtime-only state is not a promise of durable process suspension.

use super::{
    ActionResult, ActionStatus, AgentTurnExecution, AgentTurnRecord, AgentTurnState, MezError,
    Result, RuntimeSessionService,
};
use crate::runtime::control::agent_lifecycle::RuntimeAgentLifecycleTarget;
use mez_core::ids::ClientId;

/// Exact pause owner and independently fenced user operation generation.
#[derive(Debug, Clone)]
pub(super) struct HumanPause {
    target: RuntimeAgentLifecycleTarget,
    generation: u64,
}

impl HumanPause {
    /// Returns the exact retained task owner for terminal cleanup.
    pub(super) fn turn_id(&self) -> Option<&str> {
        self.target.turn_id.as_deref()
    }
}

impl RuntimeSessionService {
    /// Adopts a subsequently queued task under an already paused idle conversation.
    pub(crate) fn adopt_human_paused_work(&mut self, work: &super::ScheduledWork) {
        let Some(pane) = work.pane_id.as_deref() else {
            return;
        };
        if let Some(pause) = self.agent.human_pauses.get_mut(pane)
            && pause.target.conversation_id == work.conversation_id
        {
            if pause.target.turn_id.is_none() {
                pause.target.turn_id = Some(work.turn_id.clone());
            }
            self.agent
                .agent_scheduler
                .set_human_paused(&work.turn_id, true);
        }
    }

    /// Preserves accepted response/results without dispatching another action family.
    pub(crate) fn retain_human_paused_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: AgentTurnExecution,
        results: &[ActionResult],
    ) -> Result<AgentTurnExecution> {
        self.commit_settled_action_results_context(&turn.turn_id, results)?;
        self.agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), execution.clone());
        self.reconcile_human_pauses()?;
        Ok(execution)
    }

    /// Explicitly removes one human gate after current authority and quiescence
    /// checks. Resume supplies one trusted continuation, never redispatches a
    /// completed action or treats peer text as user authority.
    #[allow(dead_code)] // Administrative browser consumes this operation.
    pub(crate) fn resume_agent_lifecycle_target(
        &mut self,
        client: &ClientId,
        target: &RuntimeAgentLifecycleTarget,
        generation: u64,
    ) -> Result<bool> {
        let Some(pause) = self.agent.human_pauses.get(&target.pane_id).cloned() else {
            return Ok(false);
        };
        if pause.generation != generation {
            return Err(MezError::conflict("human pause generation changed"));
        }
        self.validate_agent_lifecycle_target(client, target)?;
        if target.conversation_id != pause.target.conversation_id
            || target.turn_id != pause.target.turn_id
            || target
                .process
                .as_ref()
                .zip(pause.target.process.as_ref())
                .is_some_and(|(current, original)| !current.same_incarnation(original))
        {
            return Err(MezError::conflict("paused owner changed; refresh target"));
        }
        if self.human_pause_has_issued_work(&target.pane_id, target.turn_id.as_deref()) {
            return Err(MezError::conflict(
                "agent is still pausing; issued work has not settled",
            ));
        }
        self.refresh_project_trust_store_from_disk_if_changed()?;
        if let Some(turn_id) = target.turn_id.as_deref() {
            let turn = self
                .agent_turn_ledger()
                .turn(turn_id)
                .cloned()
                .ok_or_else(|| MezError::conflict("retained pause task unavailable"))?;
            // Stage fallible fair admission while human inhibition is still
            // intact. Queue-full must not append guidance or consume approvals.
            let admitted_peer_wait = self
                .agent
                .agent_scheduler
                .waiting_turns()
                .any(|work| work.turn_id == turn_id);
            let pending_dependencies =
                self.agent_turn_executions()
                    .get(turn_id)
                    .is_some_and(|execution| {
                        (admitted_peer_wait
                            && execution.action_results.iter().any(|result| {
                                result.action_type == "wait"
                                    && result.status == ActionStatus::Running
                            }))
                            || self.execution_waiting_for_live_joined_subagents(turn_id, execution)
                    });
            let mut resumed_scheduler = self.agent.agent_scheduler.clone();
            resumed_scheduler.set_human_paused(turn_id, false);
            if !pending_dependencies {
                if resumed_scheduler
                    .blocked_turns()
                    .any(|work| work.turn_id == turn_id)
                {
                    resumed_scheduler.requeue_blocked(turn_id)?;
                } else if resumed_scheduler
                    .waiting_turns()
                    .any(|work| work.turn_id == turn_id)
                {
                    resumed_scheduler.requeue_waiting(turn_id)?;
                }
            }
            // Issued effects are settled above. New user guidance supersedes
            // unissued candidates; their old approvals must not authorize new work.
            if let Some(mut execution) = self.agent_turn_executions().get(turn_id).cloned() {
                let mut superseded = Vec::new();
                let waiting_for_children =
                    self.execution_waiting_for_live_joined_subagents(turn_id, &execution);
                for result in &mut execution.action_results {
                    if matches!(result.status, ActionStatus::Running | ActionStatus::Blocked)
                        && !(result.action_type == "wait" && admitted_peer_wait)
                        && !waiting_for_children
                    {
                        result.status = ActionStatus::Cancelled;
                        result.is_error = false;
                        result.error = None;
                        result.permission_evaluation = None;
                        result.content = vec![mez_agent::ActionContentBlock::text(
                            "Unissued candidate superseded by explicit user resume; no effect was dispatched and no approval carries forward.",
                        )];
                        superseded.push(result.clone());
                    }
                }
                self.commit_settled_action_results_context(turn_id, &superseded)?;
                execution.final_turn = false;
                execution.terminal_state = AgentTurnState::Running;
                self.agent_turn_executions_mut()
                    .insert(turn_id.to_string(), execution);
            }
            let pending = self
                .agent
                .pending_native_shell_dispatches
                .iter()
                .filter(|((owner, _), _)| owner == turn_id)
                .map(|(identity, dispatch)| (identity.clone(), dispatch.marker.clone()))
                .collect::<Vec<_>>();
            for (identity, marker) in pending {
                self.remove_running_shell_transaction(&marker);
                self.clear_shell_transaction_protocol_state(&marker);
                self.agent.pending_native_shell_dispatches.remove(&identity);
            }
            self.agent
                .pending_approved_external_actions
                .retain(|(owner, _), _| owner != turn_id);
            self.agent
                .pending_apply_patch_phases
                .retain(|key, _| !key.starts_with(&format!("{turn_id}/")));
            self.clear_blocked_agent_approvals_for_turn(turn_id);
            self.agent_turn_contexts_mut().get_mut(turn_id)
                .ok_or_else(|| MezError::invalid_state("retained task context unavailable"))?
                .append_user_event(format!("human resume {generation}"), "Continue your retained task from its current state. Do not repeat completed actions or automatically retry uncertain effects. Reevaluate unissued work under current policy; already-running children may continue.")
                .map_err(|error| MezError::invalid_state(error.to_string()))?;
            self.agent.agent_scheduler = resumed_scheduler;
            if !pending_dependencies && turn.state == AgentTurnState::Running {
                self.queue_agent_provider_task(turn_id);
            }
        }
        self.agent.human_pauses.remove(&target.pane_id);
        self.start_ready_agent_turns()?;
        Ok(true)
    }

    /// Reports dispatch inhibition for the current conversation of a pane.
    pub(crate) fn agent_is_human_paused(&self, pane: &str) -> bool {
        self.agent.human_pauses.get(pane).is_some_and(|pause| {
            self.agent_shell_store()
                .get(pane)
                .is_some_and(|session| session.session_id == pause.target.conversation_id)
        })
    }

    /// Reports inhibition by immutable turn identity, not another pane's focus.
    pub(crate) fn agent_turn_is_human_paused(&self, turn: &str) -> bool {
        self.agent
            .human_pauses
            .values()
            .any(|pause| pause.target.turn_id.as_deref() == Some(turn))
    }

    /// Checks issued owners only; pending native dispatch markers are not effects.
    fn human_pause_has_issued_work(&self, pane: &str, turn: Option<&str>) -> bool {
        let turn_work = turn.is_some_and(|turn| {
            self.agent.claimed_agent_provider_tasks.contains_key(turn)
                || self.agent.pending_agent_provider_persistence.contains(turn)
                || !self.agent_worker_attempts_for_turn(turn).is_empty()
                || self
                    .running_shell_transaction_targets_for_turn(turn)
                    .iter()
                    .any(|(marker, _)| !self.native_shell_marker_has_worker_owner(marker))
                || self
                    .integration
                    .pending_program_hook_continuations()
                    .iter()
                    .any(|work| work.turn_id == turn)
        });
        turn_work
            || self
                .agent
                .claimed_agent_compaction_tasks
                .keys()
                .any(|(owner, _)| owner == pane)
            || self.agent.claimed_agent_remember_tasks.contains_key(pane)
            || self
                .agent
                .claimed_agent_session_title_tasks
                .values()
                .any(|claim| claim.task.pane_id == pane)
    }

    /// Returns the exact resume fence owned by the retained human pause.
    pub(crate) fn agent_human_pause_generation(&self, pane: &str) -> Option<u64> {
        self.agent
            .human_pauses
            .get(pane)
            .map(|pause| pause.generation)
    }

    /// Returns truthful Pausing/Paused presentation without implying cancellation.
    #[allow(dead_code)] // Administrative browser consumes this typed status.
    pub(crate) fn agent_human_pause_status(&self, pane: &str) -> Option<&'static str> {
        let pause = self.agent.human_pauses.get(pane)?;
        if !self.agent_is_human_paused(pane) {
            return None;
        }
        Some(
            if self.human_pause_has_issued_work(pane, pause.target.turn_id.as_deref()) {
                "pausing"
            } else {
                "paused"
            },
        )
    }

    /// Admits an exact primary-owned pause; returns its resume fence.
    #[allow(dead_code)] // Used by administrative browser and deterministic tests.
    pub(crate) fn pause_agent_lifecycle_target(
        &mut self,
        client: &ClientId,
        target: &RuntimeAgentLifecycleTarget,
    ) -> Result<u64> {
        self.validate_agent_lifecycle_target(client, target)?;
        if let Some(pause) = self.agent.human_pauses.get(&target.pane_id) {
            return Ok(pause.generation);
        }
        let generation = self
            .agent
            .next_human_pause_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("human pause generation exhausted"))?;
        self.agent.next_human_pause_generation = generation;
        self.agent.human_pauses.insert(
            target.pane_id.clone(),
            HumanPause {
                target: target.clone(),
                generation,
            },
        );
        if let Some(turn) = &target.turn_id {
            self.agent.agent_scheduler.set_human_paused(turn, true);
        }
        self.reconcile_human_pauses()?;
        Ok(generation)
    }

    /// Releases capacity only after issued work settles, retaining exclusive claims.
    pub(crate) fn reconcile_human_pauses(&mut self) -> Result<()> {
        let owners = self
            .agent
            .human_pauses
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for pause in owners {
            let pane = &pause.target.pane_id;
            if self
                .agent_shell_store()
                .get(pane)
                .is_none_or(|session| session.session_id != pause.target.conversation_id)
                || pause
                    .target
                    .process
                    .as_ref()
                    .is_some_and(|process| !self.pane_process_identity_is_current(pane, process))
            {
                self.agent.human_pauses.remove(pane);
                if let Some(turn) = pause.target.turn_id.as_deref() {
                    self.agent.agent_scheduler.set_human_paused(turn, false);
                }
                continue;
            }
            let Some(turn_id) = pause.target.turn_id.as_deref() else {
                continue;
            };
            if self.human_pause_has_issued_work(&pause.target.pane_id, Some(turn_id)) {
                continue;
            }
            if self
                .agent
                .agent_scheduler
                .running_turns()
                .any(|work| work.turn_id == turn_id)
            {
                self.agent.agent_scheduler.block_running(turn_id)?;
                self.agent_turn_ledger_mut()
                    .finish_turn(turn_id, AgentTurnState::Blocked)?;
            }
            self.park_agent_turn_deadline(turn_id, super::current_unix_millis());
        }
        Ok(())
    }
}
