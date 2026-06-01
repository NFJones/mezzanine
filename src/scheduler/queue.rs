//! Queue mutation and lifecycle operations for scheduled work.
//!
//! This file owns all state-changing scheduler behavior. It delegates
//! validation to the policy module so queue operations stay focused on moving
//! work between waiting and running states.

use std::collections::VecDeque;

use crate::error::{MezError, MezErrorKind, Result};

use super::policy::validate_work;
use super::types::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, RunningWork, ScheduledWork, ScheduledWorkKind,
    SchedulerCancellation, SchedulerSnapshot,
};

impl AgentScheduler {
    /// Creates an empty scheduler with the provided concurrency limit.
    ///
    /// Returns an invalid-arguments error when the limit is zero.
    pub fn new(max_concurrent_agents: usize) -> Result<Self> {
        if max_concurrent_agents == 0 {
            return Err(MezError::invalid_args(
                "max concurrent agents must be greater than zero",
            ));
        }
        Ok(Self {
            max_concurrent_agents,
            queued: VecDeque::new(),
            running: Vec::new(),
            blocked: Vec::new(),
            last_started_agent_id: None,
        })
    }

    /// Creates an empty scheduler using the repository default concurrency
    /// limit.
    pub fn with_default_limit() -> Self {
        Self {
            max_concurrent_agents: DEFAULT_MAX_CONCURRENT_AGENTS,
            queued: VecDeque::new(),
            running: Vec::new(),
            blocked: Vec::new(),
            last_started_agent_id: None,
        }
    }

    /// Updates the concurrency limit without cancelling already running work.
    ///
    /// Returns an invalid-arguments error when the new limit is zero.
    pub fn set_max_concurrent_agents(&mut self, max_concurrent_agents: usize) -> Result<()> {
        if max_concurrent_agents == 0 {
            return Err(MezError::invalid_args(
                "max concurrent agents must be greater than zero",
            ));
        }
        self.max_concurrent_agents = max_concurrent_agents;
        Ok(())
    }

    /// Adds a new turn to the scheduler queue.
    ///
    /// Returns an error when the work is malformed or when another queued or
    /// running turn already uses the same turn id.
    pub fn enqueue(&mut self, work: ScheduledWork) -> Result<()> {
        validate_work(&work)?;
        if self
            .queued
            .iter()
            .any(|queued| queued.turn_id == work.turn_id)
            || self
                .running
                .iter()
                .any(|running| running.turn_id == work.turn_id)
            || self
                .blocked
                .iter()
                .any(|blocked| blocked.turn_id == work.turn_id)
        {
            return Err(MezError::conflict(
                "scheduled turn id is already queued, running, or blocked",
            ));
        }
        self.queued.push_back(work);
        Ok(())
    }

    /// Starts the next queued turn that satisfies fairness and pane policy.
    ///
    /// Runnable work owned by a different agent than the most recently started
    /// agent is preferred when available, and pane-conflicted turns are skipped
    /// without preventing later runnable work from starting.
    pub fn start_ready(&mut self) -> Option<RunningWork> {
        if self.running.len() + self.blocked.len() >= self.max_concurrent_agents {
            return None;
        }
        self.start_ready_candidate(true)
            .or_else(|| self.start_ready_candidate(false))
    }

    /// Marks a running turn complete and removes it from active scheduler state.
    ///
    /// Returns a not-found error when no running turn has the requested id.
    pub fn complete(&mut self, turn_id: &str) -> Result<RunningWork> {
        let index = self
            .running
            .iter()
            .position(|running| running.turn_id == turn_id)
            .ok_or_else(|| MezError::new(MezErrorKind::NotFound, "turn not found"))?;
        Ok(self.running.remove(index))
    }

    /// Moves a running turn into blocked state while retaining its global
    /// concurrency reservation.
    ///
    /// Blocked work still participates in agent and pane exclusivity checks so a
    /// waiting turn cannot be bypassed by another shell-capable turn that would
    /// write to the same pane.
    pub fn block_running(&mut self, turn_id: &str) -> Result<RunningWork> {
        let index = self
            .running
            .iter()
            .position(|running| running.turn_id == turn_id)
            .ok_or_else(|| MezError::new(MezErrorKind::NotFound, "turn not found"))?;
        let work = self.running.remove(index);
        self.blocked.push(work.clone());
        Ok(work)
    }

    /// Moves a blocked turn back to running state.
    ///
    /// Approved continuations are resumptions of already-started user work. The
    /// scheduler reserves capacity while work is blocked so resuming cannot
    /// exceed the configured concurrency limit.
    pub fn resume_blocked(&mut self, turn_id: &str) -> Result<RunningWork> {
        let index = self
            .blocked
            .iter()
            .position(|blocked| blocked.turn_id == turn_id)
            .ok_or_else(|| MezError::new(MezErrorKind::NotFound, "turn not found"))?;
        let work = self.blocked.remove(index);
        self.running.push(work.clone());
        Ok(work)
    }

    /// Cancels queued or running work by turn id.
    ///
    /// Returns the cancelled work and whether it had already started, or a
    /// not-found error when the turn id is unknown.
    pub fn cancel(&mut self, turn_id: &str) -> Result<SchedulerCancellation> {
        if let Some(index) = self
            .queued
            .iter()
            .position(|queued| queued.turn_id == turn_id)
        {
            let work = self.queued.remove(index).ok_or_else(|| {
                MezError::invalid_state("queued scheduler work disappeared during cancellation")
            })?;
            return Ok(SchedulerCancellation::Queued(work));
        }

        if let Some(index) = self
            .running
            .iter()
            .position(|running| running.turn_id == turn_id)
        {
            return Ok(SchedulerCancellation::Running(self.running.remove(index)));
        }

        if let Some(index) = self
            .blocked
            .iter()
            .position(|blocked| blocked.turn_id == turn_id)
        {
            return Ok(SchedulerCancellation::Blocked(self.blocked.remove(index)));
        }

        Err(MezError::new(MezErrorKind::NotFound, "turn not found"))
    }

    /// Returns queue and running counters without exposing mutable scheduler
    /// storage.
    pub fn snapshot(&self) -> SchedulerSnapshot {
        SchedulerSnapshot {
            queued: self.queued.len(),
            running: self.running.len(),
            blocked: self.blocked.len(),
            max_concurrent_agents: self.max_concurrent_agents,
        }
    }

    /// Iterates queued turns in their current fairness order.
    pub fn queued_turns(&self) -> impl Iterator<Item = &ScheduledWork> {
        self.queued.iter()
    }

    /// Iterates currently running turns.
    pub fn running_turns(&self) -> impl Iterator<Item = &RunningWork> {
        self.running.iter()
    }

    /// Iterates turns blocked on external input.
    pub fn blocked_turns(&self) -> impl Iterator<Item = &RunningWork> {
        self.blocked.iter()
    }

    /// Runs the can start operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn can_start(&self, work: &ScheduledWork) -> bool {
        if self
            .running
            .iter()
            .chain(self.blocked.iter())
            .any(|running| running.agent_id == work.agent_id)
        {
            return false;
        }
        if work.kind != ScheduledWorkKind::ShellCapable {
            return true;
        }
        let Some(pane_id) = &work.pane_id else {
            return false;
        };
        !self
            .running
            .iter()
            .chain(self.blocked.iter())
            .any(|running| {
                running.kind == ScheduledWorkKind::ShellCapable
                    && running.pane_id.as_deref() == Some(pane_id.as_str())
            })
    }

    /// Runs the start ready candidate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn start_ready_candidate(&mut self, prefer_new_agent: bool) -> Option<RunningWork> {
        let last_started = self.last_started_agent_id.as_deref();
        let index = self.queued.iter().position(|work| {
            self.can_start(work)
                && (!prefer_new_agent || Some(work.agent_id.as_str()) != last_started)
        })?;
        let work = self.queued.remove(index)?;
        let running = RunningWork {
            turn_id: work.turn_id,
            agent_id: work.agent_id,
            pane_id: work.pane_id,
            kind: work.kind,
        };
        self.last_started_agent_id = Some(running.agent_id.clone());
        self.running.push(running.clone());
        Some(running)
    }
}
