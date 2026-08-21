//! Queue mutation and lifecycle operations for scheduled work.
//!
//! This file owns all state-changing scheduler behavior. It delegates
//! validation to the policy module so queue operations stay focused on moving
//! work between waiting and running states.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use super::error::{SchedulerError, SchedulerErrorKind, SchedulerResult};
use super::policy::validate_work;
use super::types::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, DEFAULT_MAX_QUEUED_BYTES,
    DEFAULT_MAX_QUEUED_TURNS, QueuedWork, RunningWork, ScheduledWork, ScheduledWorkKind,
    SchedulerCancellation, SchedulerSnapshot, SchedulerTurnState,
};

impl AgentScheduler {
    /// Creates an empty scheduler with the provided concurrency limit.
    ///
    /// Returns an invalid-arguments error when the limit is zero.
    pub fn new(max_concurrent_agents: usize) -> SchedulerResult<Self> {
        Self::with_limits(
            max_concurrent_agents,
            DEFAULT_MAX_QUEUED_TURNS,
            DEFAULT_MAX_QUEUED_BYTES,
        )
    }

    /// Creates an empty scheduler with explicit concurrency and queue budgets.
    pub fn with_limits(
        max_concurrent_agents: usize,
        max_queued_turns: usize,
        max_queued_bytes: usize,
    ) -> SchedulerResult<Self> {
        if max_concurrent_agents == 0 || max_queued_turns == 0 || max_queued_bytes == 0 {
            return Err(SchedulerError::invalid_args(
                "scheduler concurrency and queue limits must be greater than zero",
            ));
        }
        Ok(Self {
            max_concurrent_agents,
            max_queued_turns,
            max_queued_bytes,
            queued: HashMap::new(),
            queued_order: Default::default(),
            queued_by_agent: HashMap::new(),
            queued_by_pane: HashMap::new(),
            ready_by_agent: HashMap::new(),
            ready_order: Default::default(),
            next_sequence: 0,
            queued_bytes: 0,
            turn_states: HashMap::new(),
            claimed_agents: HashSet::new(),
            claimed_panes: HashSet::new(),
            running: HashMap::new(),
            blocked: HashMap::new(),
            waiting: HashMap::new(),
            reacquiring: HashMap::new(),
            last_started_agent_id: None,
            admission_rejections: 0,
            readiness_checks: 0,
        })
    }

    /// Creates an empty scheduler using the repository default concurrency
    /// limit.
    pub fn with_default_limit() -> Self {
        Self::with_limits(
            DEFAULT_MAX_CONCURRENT_AGENTS,
            DEFAULT_MAX_QUEUED_TURNS,
            DEFAULT_MAX_QUEUED_BYTES,
        )
        .expect("default scheduler limits are non-zero")
    }

    /// Updates the concurrency limit without cancelling already running work.
    ///
    /// Returns an invalid-arguments error when the new limit is zero.
    pub fn set_max_concurrent_agents(
        &mut self,
        max_concurrent_agents: usize,
    ) -> SchedulerResult<()> {
        if max_concurrent_agents == 0 {
            return Err(SchedulerError::invalid_args(
                "max concurrent agents must be greater than zero",
            ));
        }
        self.max_concurrent_agents = max_concurrent_agents;
        Ok(())
    }

    /// Updates queue admission limits without dropping already queued work.
    pub fn set_queue_limits(
        &mut self,
        max_queued_turns: usize,
        max_queued_bytes: usize,
    ) -> SchedulerResult<()> {
        if max_queued_turns == 0 || max_queued_bytes == 0 {
            return Err(SchedulerError::invalid_args(
                "scheduler queue limits must be greater than zero",
            ));
        }
        self.max_queued_turns = max_queued_turns;
        self.max_queued_bytes = max_queued_bytes;
        Ok(())
    }

    /// Adds a new turn to the scheduler queue.
    ///
    /// Returns an error when the work is malformed or when another queued or
    /// running turn already uses the same turn id.
    pub fn enqueue(&mut self, work: ScheduledWork) -> SchedulerResult<()> {
        validate_work(&work)?;
        if self.turn_states.contains_key(&work.turn_id) {
            return Err(SchedulerError::conflict(
                "scheduled turn id is already queued, running, blocked, or waiting",
            ));
        }
        self.enqueue_validated(work, SchedulerTurnState::Queued)
    }

    /// Starts the next queued turn that satisfies fairness and pane policy.
    ///
    /// Runnable work owned by a different agent than the most recently started
    /// agent is preferred when available, and pane-conflicted turns are skipped
    /// without preventing later runnable work from starting.
    pub fn start_ready(&mut self) -> Option<RunningWork> {
        self.start_ready_where(|_| true)
    }

    /// Starts the next queued turn accepted by runtime-specific readiness.
    ///
    /// Scheduler fairness and pane claims remain owned here, while callers may
    /// impose a transient execution-surface gate without removing or rotating
    /// rejected queue entries. A rejected candidate does not prevent a later
    /// independent agent from starting.
    pub fn start_ready_where(
        &mut self,
        mut predicate: impl FnMut(&ScheduledWork) -> bool,
    ) -> Option<RunningWork> {
        if self.active_capacity_used() >= self.max_concurrent_agents {
            return None;
        }
        self.start_ready_candidate_where(true, &mut predicate)
            .or_else(|| self.start_ready_candidate_where(false, &mut predicate))
    }

    /// Marks a running turn complete and removes it from active scheduler state.
    ///
    /// Returns a not-found error when no running turn has the requested id.
    pub fn complete(&mut self, turn_id: &str) -> SchedulerResult<RunningWork> {
        let work = self
            .running
            .remove(turn_id)
            .ok_or_else(|| SchedulerError::new(SchedulerErrorKind::NotFound, "turn not found"))?;
        self.turn_states.remove(turn_id);
        self.release_claims(&work);
        Ok(work)
    }

    /// Moves a running turn into blocked state and releases provider capacity.
    ///
    /// Blocked work still participates in agent and pane exclusivity checks so a
    /// waiting turn cannot be bypassed by another shell-capable turn that would
    /// write to the same pane.
    pub fn block_running(&mut self, turn_id: &str) -> SchedulerResult<RunningWork> {
        let work = self
            .running
            .remove(turn_id)
            .ok_or_else(|| SchedulerError::new(SchedulerErrorKind::NotFound, "turn not found"))?;
        self.blocked.insert(turn_id.to_string(), work.clone());
        self.turn_states
            .insert(turn_id.to_string(), SchedulerTurnState::Blocked);
        Ok(work)
    }

    /// Queues an externally blocked turn for fair provider-capacity reacquisition.
    ///
    /// The turn re-enters the normal ready queue while a private claim keeps
    /// its agent and pane exclusive until it starts or is cancelled.
    pub fn requeue_blocked(&mut self, turn_id: &str) -> SchedulerResult<ScheduledWork> {
        let work =
            self.blocked.get(turn_id).cloned().ok_or_else(|| {
                SchedulerError::new(SchedulerErrorKind::NotFound, "turn not found")
            })?;
        let scheduled = ScheduledWork {
            turn_id: work.turn_id.clone(),
            conversation_id: work.conversation_id.clone(),
            agent_id: work.agent_id.clone(),
            pane_id: work.pane_id.clone(),
            kind: work.kind,
        };
        self.ensure_queue_capacity(&scheduled)?;
        self.blocked.remove(turn_id);
        self.reacquiring.insert(turn_id.to_string(), work);
        self.insert_queued(scheduled.clone(), SchedulerTurnState::Reacquiring);
        Ok(scheduled)
    }

    /// Moves a running parent into dependency-waiting state and releases its
    /// provider-capacity slot.
    ///
    /// Waiting work retains agent and shell-pane exclusivity so unrelated work
    /// cannot take over its lifecycle owner while a routed worker or joined
    /// subagent is outstanding.
    pub fn wait_running(&mut self, turn_id: &str) -> SchedulerResult<RunningWork> {
        let work = self
            .running
            .remove(turn_id)
            .ok_or_else(|| SchedulerError::new(SchedulerErrorKind::NotFound, "turn not found"))?;
        self.waiting.insert(turn_id.to_string(), work.clone());
        self.turn_states
            .insert(turn_id.to_string(), SchedulerTurnState::Waiting);
        Ok(work)
    }

    /// Queues a dependency-waiting parent for fair capacity reacquisition.
    ///
    /// The parent re-enters the normal ready queue while a private claim keeps
    /// its agent and pane exclusive until it starts or is cancelled.
    pub fn requeue_waiting(&mut self, turn_id: &str) -> SchedulerResult<ScheduledWork> {
        let work =
            self.waiting.get(turn_id).cloned().ok_or_else(|| {
                SchedulerError::new(SchedulerErrorKind::NotFound, "turn not found")
            })?;
        let scheduled = ScheduledWork {
            turn_id: work.turn_id.clone(),
            conversation_id: work.conversation_id.clone(),
            agent_id: work.agent_id.clone(),
            pane_id: work.pane_id.clone(),
            kind: work.kind,
        };
        self.ensure_queue_capacity(&scheduled)?;
        self.waiting.remove(turn_id);
        self.reacquiring.insert(turn_id.to_string(), work);
        self.insert_queued(scheduled.clone(), SchedulerTurnState::Reacquiring);
        Ok(scheduled)
    }

    /// Moves a blocked turn back to running state.
    ///
    /// Approved continuations should normally use [`Self::requeue_blocked`] so
    /// they participate in fairness. This immediate path is retained for
    /// callers that have already established available provider capacity.
    pub fn resume_blocked(&mut self, turn_id: &str) -> SchedulerResult<RunningWork> {
        if self.active_capacity_used() >= self.max_concurrent_agents {
            return Err(SchedulerError::invalid_state(
                "provider capacity is unavailable for blocked turn resumption",
            ));
        }
        let work = self
            .blocked
            .remove(turn_id)
            .ok_or_else(|| SchedulerError::new(SchedulerErrorKind::NotFound, "turn not found"))?;
        self.running.insert(turn_id.to_string(), work.clone());
        self.turn_states
            .insert(turn_id.to_string(), SchedulerTurnState::Running);
        Ok(work)
    }

    /// Cancels queued or running work by turn id.
    ///
    /// Returns the cancelled work and whether it had already started, or a
    /// not-found error when the turn id is unknown.
    pub fn cancel(&mut self, turn_id: &str) -> SchedulerResult<SchedulerCancellation> {
        if self.queued.contains_key(turn_id) {
            let state = self.turn_states.get(turn_id).copied();
            let work = self.remove_queued(turn_id).ok_or_else(|| {
                SchedulerError::invalid_state(
                    "queued scheduler work disappeared during cancellation",
                )
            })?;
            if state == Some(SchedulerTurnState::Reacquiring)
                && let Some(claim) = self.reacquiring.remove(turn_id)
            {
                self.release_claims(&claim);
            }
            self.turn_states.remove(turn_id);
            return Ok(SchedulerCancellation::Queued(work));
        }

        if let Some(work) = self.running.remove(turn_id) {
            self.turn_states.remove(turn_id);
            self.release_claims(&work);
            return Ok(SchedulerCancellation::Running(work));
        }

        if let Some(work) = self.blocked.remove(turn_id) {
            self.turn_states.remove(turn_id);
            self.release_claims(&work);
            return Ok(SchedulerCancellation::Blocked(work));
        }

        if let Some(work) = self.waiting.remove(turn_id) {
            self.turn_states.remove(turn_id);
            self.release_claims(&work);
            return Ok(SchedulerCancellation::Waiting(work));
        }

        Err(SchedulerError::new(
            SchedulerErrorKind::NotFound,
            "turn not found",
        ))
    }

    /// Returns queue and running counters without exposing mutable scheduler
    /// storage.
    pub fn snapshot(&self) -> SchedulerSnapshot {
        let oldest_queued_age_ms = self
            .queued_order
            .first_key_value()
            .and_then(|(_, turn_id)| self.queued.get(turn_id))
            .map(|queued| {
                u64::try_from(queued.enqueued_at.elapsed().as_millis()).unwrap_or(u64::MAX)
            })
            .unwrap_or(0);
        SchedulerSnapshot {
            queued: self.queued.len(),
            queued_bytes: self.queued_bytes,
            oldest_queued_age_ms,
            running: self.running.len(),
            blocked: self.blocked.len(),
            waiting: self.waiting.len(),
            reacquiring: self.reacquiring.len(),
            active_capacity_used: self.active_capacity_used(),
            max_concurrent_agents: self.max_concurrent_agents,
            max_queued_turns: self.max_queued_turns,
            max_queued_bytes: self.max_queued_bytes,
            admission_rejections: self.admission_rejections,
            readiness_checks: self.readiness_checks,
        }
    }

    /// Returns the number of turns that currently own provider capacity.
    pub fn active_capacity_used(&self) -> usize {
        self.running.len()
    }

    /// Iterates queued turns in their current fairness order.
    pub fn queued_turns(&self) -> impl Iterator<Item = &ScheduledWork> {
        self.queued_order
            .values()
            .filter_map(|turn_id| self.queued.get(turn_id).map(|queued| &queued.work))
    }

    /// Iterates currently running turns.
    pub fn running_turns(&self) -> impl Iterator<Item = &RunningWork> {
        self.running.values()
    }

    /// Iterates turns blocked on external input.
    pub fn blocked_turns(&self) -> impl Iterator<Item = &RunningWork> {
        self.blocked.values()
    }

    /// Iterates parent turns waiting for routed or joined dependent work.
    pub fn waiting_turns(&self) -> impl Iterator<Item = &RunningWork> {
        self.waiting.values()
    }

    /// Iterates waiting parents queued to reacquire provider capacity.
    pub fn reacquiring_turns(&self) -> impl Iterator<Item = &RunningWork> {
        self.reacquiring.values()
    }

    /// Runs the can start operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn can_start(&self, work: &ScheduledWork) -> bool {
        let owns_reacquiring_claim =
            self.turn_states.get(&work.turn_id) == Some(&SchedulerTurnState::Reacquiring);
        if self.claimed_agents.contains(&work.agent_id) && !owns_reacquiring_claim {
            return false;
        }
        if work.kind != ScheduledWorkKind::ShellCapable {
            return true;
        }
        let Some(pane_id) = &work.pane_id else {
            return false;
        };
        !self.claimed_panes.contains(pane_id) || owns_reacquiring_claim
    }

    /// Runs the start ready candidate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn start_ready_candidate_where(
        &mut self,
        prefer_new_agent: bool,
        predicate: &mut impl FnMut(&ScheduledWork) -> bool,
    ) -> Option<RunningWork> {
        let candidate = self
            .ready_order
            .iter()
            .find(|(_, agent_id, turn_id)| {
                (!prefer_new_agent
                    || self.last_started_agent_id.as_deref() != Some(agent_id.as_str()))
                    && self
                        .queued
                        .get(turn_id.as_str())
                        .is_some_and(|queued| predicate(&queued.work))
            })?
            .clone();
        let work = self.remove_queued(&candidate.2)?;
        let reacquiring =
            self.turn_states.get(&work.turn_id) == Some(&SchedulerTurnState::Reacquiring);
        let running = RunningWork {
            turn_id: work.turn_id,
            conversation_id: work.conversation_id,
            agent_id: work.agent_id,
            pane_id: work.pane_id,
            kind: work.kind,
        };
        self.last_started_agent_id = Some(running.agent_id.clone());
        if reacquiring {
            self.reacquiring.remove(&running.turn_id);
        } else {
            self.claim_work(&running);
        }
        self.turn_states
            .insert(running.turn_id.clone(), SchedulerTurnState::Running);
        self.running
            .insert(running.turn_id.clone(), running.clone());
        self.refresh_affected_readiness(&running);
        Some(running)
    }

    fn enqueue_validated(
        &mut self,
        work: ScheduledWork,
        state: SchedulerTurnState,
    ) -> SchedulerResult<()> {
        self.ensure_queue_capacity(&work)?;
        self.insert_queued(work, state);
        Ok(())
    }

    fn ensure_queue_capacity(&mut self, work: &ScheduledWork) -> SchedulerResult<()> {
        let estimated_bytes = estimated_work_bytes(work);
        if self.queued.len() >= self.max_queued_turns
            || self.queued_bytes.saturating_add(estimated_bytes) > self.max_queued_bytes
        {
            self.admission_rejections = self.admission_rejections.saturating_add(1);
            return Err(SchedulerError::invalid_state(
                "scheduler queue admission limit exceeded",
            ));
        }
        Ok(())
    }

    fn insert_queued(&mut self, work: ScheduledWork, state: SchedulerTurnState) {
        let sequence = self.next_available_sequence();
        let turn_id = work.turn_id.clone();
        let agent_id = work.agent_id.clone();
        let pane_id = (work.kind == ScheduledWorkKind::ShellCapable)
            .then(|| work.pane_id.clone())
            .flatten();
        let estimated_bytes = estimated_work_bytes(&work);
        self.queued_bytes = self.queued_bytes.saturating_add(estimated_bytes);
        self.queued_order.insert(sequence, turn_id.clone());
        self.queued_by_agent
            .entry(agent_id.clone())
            .or_default()
            .insert((sequence, turn_id.clone()));
        if let Some(pane_id) = pane_id {
            self.queued_by_pane
                .entry(pane_id)
                .or_default()
                .insert(turn_id.clone());
        }
        self.turn_states.insert(turn_id.clone(), state);
        self.queued.insert(
            turn_id,
            QueuedWork {
                work,
                sequence,
                enqueued_at: Instant::now(),
                estimated_bytes,
            },
        );
        self.refresh_ready_for_agent(&agent_id);
    }

    fn remove_queued(&mut self, turn_id: &str) -> Option<ScheduledWork> {
        let queued = self.queued.remove(turn_id)?;
        let agent_id = queued.work.agent_id.clone();
        self.queued_order.remove(&queued.sequence);
        if let Some(entries) = self.queued_by_agent.get_mut(&agent_id) {
            entries.remove(&(queued.sequence, turn_id.to_string()));
            if entries.is_empty() {
                self.queued_by_agent.remove(&agent_id);
            }
        }
        if queued.work.kind == ScheduledWorkKind::ShellCapable
            && let Some(pane_id) = queued.work.pane_id.as_ref()
            && let Some(turn_ids) = self.queued_by_pane.get_mut(pane_id)
        {
            turn_ids.remove(turn_id);
            if turn_ids.is_empty() {
                self.queued_by_pane.remove(pane_id);
            }
        }
        self.queued_bytes = self.queued_bytes.saturating_sub(queued.estimated_bytes);
        self.remove_ready_agent(&agent_id);
        self.refresh_ready_for_agent(&agent_id);
        Some(queued.work)
    }

    fn refresh_ready_for_agent(&mut self, agent_id: &str) {
        self.remove_ready_agent(agent_id);
        let entries = self
            .queued_by_agent
            .get(agent_id)
            .map(|entries| entries.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut candidate = None;
        for (sequence, turn_id) in entries {
            self.readiness_checks = self.readiness_checks.saturating_add(1);
            if self
                .queued
                .get(&turn_id)
                .is_some_and(|queued| self.can_start(&queued.work))
            {
                candidate = Some((sequence, turn_id));
                break;
            }
        }
        if let Some((sequence, turn_id)) = candidate {
            self.ready_by_agent
                .insert(agent_id.to_string(), (sequence, turn_id.clone()));
            self.ready_order
                .insert((sequence, agent_id.to_string(), turn_id));
        }
    }

    fn remove_ready_agent(&mut self, agent_id: &str) {
        if let Some((sequence, turn_id)) = self.ready_by_agent.remove(agent_id) {
            self.ready_order
                .remove(&(sequence, agent_id.to_string(), turn_id));
        }
    }

    fn claim_work(&mut self, work: &RunningWork) {
        self.claimed_agents.insert(work.agent_id.clone());
        if work.kind == ScheduledWorkKind::ShellCapable
            && let Some(pane_id) = work.pane_id.as_ref()
        {
            self.claimed_panes.insert(pane_id.clone());
        }
    }

    fn release_claims(&mut self, work: &RunningWork) {
        self.claimed_agents.remove(&work.agent_id);
        if work.kind == ScheduledWorkKind::ShellCapable
            && let Some(pane_id) = work.pane_id.as_ref()
        {
            self.claimed_panes.remove(pane_id);
        }
        self.refresh_affected_readiness(work);
    }

    fn refresh_affected_readiness(&mut self, work: &RunningWork) {
        let mut agents = HashSet::from([work.agent_id.clone()]);
        if work.kind == ScheduledWorkKind::ShellCapable
            && let Some(pane_id) = work.pane_id.as_ref()
            && let Some(turn_ids) = self.queued_by_pane.get(pane_id)
        {
            agents.extend(turn_ids.iter().filter_map(|turn_id| {
                self.queued
                    .get(turn_id)
                    .map(|queued| queued.work.agent_id.clone())
            }));
        }
        for agent_id in agents {
            self.refresh_ready_for_agent(&agent_id);
        }
    }

    fn next_available_sequence(&mut self) -> u64 {
        loop {
            let sequence = self.next_sequence;
            self.next_sequence = self.next_sequence.wrapping_add(1);
            if !self.queued_order.contains_key(&sequence) {
                return sequence;
            }
        }
    }
}

fn estimated_work_bytes(work: &ScheduledWork) -> usize {
    std::mem::size_of::<ScheduledWork>()
        .saturating_add(work.turn_id.len())
        .saturating_add(work.conversation_id.len())
        .saturating_add(work.agent_id.len())
        .saturating_add(work.pane_id.as_deref().map(str::len).unwrap_or(0))
}
