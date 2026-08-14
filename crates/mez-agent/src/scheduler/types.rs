//! Shared scheduler data types.
//!
//! These types define the public contract for queued, active, blocked, and
//! dependency-waiting work while keeping mutable queue storage owned by the
//! scheduler implementation. Provider capacity is owned only by running work;
//! blocked and dependency-waiting turns retain lifecycle and pane claims
//! without consuming provider capacity.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Instant;

/// Default upper bound for concurrently running agent turns.
pub const DEFAULT_MAX_CONCURRENT_AGENTS: usize = 4;
/// Default upper bound for turns waiting in the scheduler queue.
pub const DEFAULT_MAX_QUEUED_TURNS: usize = 256;
/// Default upper bound for estimated bytes retained by queued turns.
pub const DEFAULT_MAX_QUEUED_BYTES: usize = 4 * 1024 * 1024;

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
    /// Immutable conversation that owns this scheduled work.
    pub conversation_id: String,
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
    /// Immutable conversation that owns this running work.
    pub conversation_id: String,
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
    /// Estimated bytes retained by turns waiting to run.
    pub queued_bytes: usize,
    /// Age of the oldest queued turn in milliseconds.
    pub oldest_queued_age_ms: u64,
    /// Number of turns currently running.
    pub running: usize,
    /// Number of turns blocked on external input while retaining agent and
    /// pane ownership without provider capacity.
    pub blocked: usize,
    /// Number of parent turns waiting for dependent work without provider
    /// capacity.
    pub waiting: usize,
    /// Number of waiting parents queued to reacquire provider capacity.
    pub reacquiring: usize,
    /// Number of provider-capacity slots currently owned.
    pub active_capacity_used: usize,
    /// Configured maximum concurrent agent turns.
    pub max_concurrent_agents: usize,
    /// Configured maximum number of queued turns.
    pub max_queued_turns: usize,
    /// Configured maximum estimated bytes retained by queued turns.
    pub max_queued_bytes: usize,
    /// Number of admissions rejected by queue count or byte budgets.
    pub admission_rejections: u64,
    /// Number of queued candidates evaluated by readiness selection.
    pub readiness_checks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SchedulerTurnState {
    Queued,
    Running,
    Blocked,
    Waiting,
    Reacquiring,
}

/// One queued work item plus bounded-admission and ordering metadata.
#[derive(Debug, Clone)]
pub(super) struct QueuedWork {
    /// Work retained until scheduler policy admits it.
    pub(super) work: ScheduledWork,
    /// Monotonic order used for stable fairness and indexed removal.
    pub(super) sequence: u64,
    /// Monotonic enqueue instant used for queue-age diagnostics.
    pub(super) enqueued_at: Instant,
    /// Estimated retained bytes charged against the queue budget.
    pub(super) estimated_bytes: usize,
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
    /// The requested parent turn is waiting for dependent work.
    Waiting(RunningWork),
}

/// Fair scheduler for agent turns and exclusive shell-pane access.
#[derive(Debug, Clone)]
pub struct AgentScheduler {
    /// Stores the max concurrent agents value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_concurrent_agents: usize,
    /// Maximum number of turns retained in the waiting queue.
    pub(super) max_queued_turns: usize,
    /// Maximum estimated bytes retained in the waiting queue.
    pub(super) max_queued_bytes: usize,
    /// Queued work indexed by turn id for constant-time lookup and removal.
    pub(super) queued: HashMap<String, QueuedWork>,
    /// Global stable queue order indexed by monotonic sequence.
    pub(super) queued_order: BTreeMap<u64, String>,
    /// Queued sequence/id pairs grouped by agent for targeted readiness refresh.
    pub(super) queued_by_agent: HashMap<String, BTreeSet<(u64, String)>>,
    /// Queued turn ids grouped by shell pane for targeted claim refresh.
    pub(super) queued_by_pane: HashMap<String, HashSet<String>>,
    /// Current earliest runnable sequence/id pair for each agent.
    pub(super) ready_by_agent: HashMap<String, (u64, String)>,
    /// Earliest runnable work per agent in global fairness order.
    pub(super) ready_order: BTreeSet<(u64, String, String)>,
    /// Next monotonic queue sequence.
    pub(super) next_sequence: u64,
    /// Estimated bytes currently retained by queued work.
    pub(super) queued_bytes: usize,
    /// Lifecycle state indexed by turn id for duplicate detection.
    pub(super) turn_states: HashMap<String, SchedulerTurnState>,
    /// Agents retaining running, blocked, waiting, or reacquiring claims.
    pub(super) claimed_agents: HashSet<String>,
    /// Shell panes retaining running, blocked, waiting, or reacquiring claims.
    pub(super) claimed_panes: HashSet<String>,
    /// Stores the running value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) running: HashMap<String, RunningWork>,
    /// Stores the blocked value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) blocked: HashMap<String, RunningWork>,
    /// Parent turns waiting on routed workers or joined subagents.
    ///
    /// Waiting work retains agent and pane exclusivity but does not consume a
    /// provider-capacity slot.
    pub(super) waiting: HashMap<String, RunningWork>,
    /// Pane and agent claims retained while a waiting parent is queued for fair
    /// provider-capacity reacquisition.
    pub(super) reacquiring: HashMap<String, RunningWork>,
    /// Stores the last started agent id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) last_started_agent_id: Option<String>,
    /// Count of admissions rejected by configured queue budgets.
    pub(super) admission_rejections: u64,
    /// Count of queue candidates evaluated for readiness.
    pub(super) readiness_checks: u64,
}

impl Default for AgentScheduler {
    fn default() -> Self {
        Self::with_default_limit()
    }
}
