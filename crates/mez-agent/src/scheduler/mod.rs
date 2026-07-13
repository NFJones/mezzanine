//! Agent and pane scheduling primitives.
//!
//! The live runtime drives scheduling decisions from file-descriptor readiness
//! and model-provider futures. This module keeps the fairness and exclusivity
//! policy testable without a daemon: one turn per agent, at most the configured
//! session concurrency, and no two shell-capable turns writing to the same pane.

mod error;

/// Exposes the policy module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod policy;
/// Exposes the queue module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod queue;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use error::{SchedulerError, SchedulerErrorKind, SchedulerResult};
pub use policy::runnable_agent_ids;
pub use types::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, RunningWork, ScheduledWork, ScheduledWorkKind,
    SchedulerCancellation, SchedulerSnapshot,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
