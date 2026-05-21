//! Shared scheduler data types.
//!
//! These types define the public contract for queued and running work while
//! keeping mutable queue storage owned by the scheduler implementation.

use std::collections::VecDeque;

/// Default upper bound for concurrently running agent turns.
pub const DEFAULT_MAX_CONCURRENT_AGENTS: usize = 4;

/// Describes how a scheduled turn interacts with panes and background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledWorkKind {
    /// Work that may write to a shell pane and therefore needs pane exclusivity.
    ShellCapable,
    /// Work that only plans and does not claim exclusive pane access.
    PlanningOnly,
    /// Local in-process message handling that is scheduled with agent fairness.
    LocalMessage,
    /// Background task work that is not tied to shell-pane writes.
    BackgroundTask,
}

/// A queued unit of agent work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledWork {
    /// Stable turn identifier used for completion and cancellation.
    pub turn_id: String,
    /// Agent that owns the turn.
    pub agent_id: String,
    /// Optional pane claimed by shell-capable work.
    pub pane_id: Option<String>,
    /// Scheduling behavior for this turn.
    pub kind: ScheduledWorkKind,
}

/// A unit of work that has passed scheduler policy and is currently running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningWork {
    /// Stable turn identifier used for completion and cancellation.
    pub turn_id: String,
    /// Agent that owns the turn.
    pub agent_id: String,
    /// Optional pane claimed by shell-capable work.
    pub pane_id: Option<String>,
    /// Scheduling behavior for this turn.
    pub kind: ScheduledWorkKind,
}

/// Lightweight counters describing scheduler occupancy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerSnapshot {
    /// Number of turns waiting to run.
    pub queued: usize,
    /// Number of turns currently running.
    pub running: usize,
    /// Number of turns blocked on external input while retaining pane ownership.
    pub blocked: usize,
    /// Configured maximum concurrent agent turns.
    pub max_concurrent_agents: usize,
}

/// Work returned by a scheduler cancellation operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerCancellation {
    /// The requested turn was still waiting in the queue.
    Queued(ScheduledWork),
    /// The requested turn had already started.
    Running(RunningWork),
    /// The requested turn is blocked on external input.
    Blocked(RunningWork),
}

/// Fair scheduler for agent turns and exclusive shell-pane access.
#[derive(Debug, Clone)]
pub struct AgentScheduler {
    /// Stores the max concurrent agents value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_concurrent_agents: usize,
    /// Stores the queued value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) queued: VecDeque<ScheduledWork>,
    /// Stores the running value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) running: Vec<RunningWork>,
    /// Stores the blocked value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) blocked: Vec<RunningWork>,
    /// Stores the last started agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) last_started_agent_id: Option<String>,
}
