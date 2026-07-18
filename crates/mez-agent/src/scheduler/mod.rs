//! Agent and pane scheduling primitives.
//!
//! The live runtime drives scheduling decisions from file-descriptor readiness
//! and model-provider futures. This module keeps fairness, exclusivity, and
//! provider-retry phase decisions testable without a daemon: one turn per
//! agent, at most the configured session concurrency, no two shell-capable
//! turns writing to the same pane, and no timer or dispatch effect treated as
//! successful until the product reports its observed result.

mod error;

/// Exposes the policy module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod policy;
mod provider_retry;
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
pub use provider_retry::{
    ProviderRetryDispatchResult, ProviderRetryEffect, ProviderRetryEvent, ProviderRetryRecovery,
    ProviderRetryRecoveryResult, ProviderRetryScheduler, ProviderRetryTransition,
};
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
