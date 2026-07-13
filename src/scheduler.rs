//! Transitional product facade for agent-owned scheduler contracts.
//!
//! Runtime consumers still use this module while the composition root migrates
//! to direct `mez_agent` imports.

pub use mez_agent::{
    AgentScheduler, DEFAULT_MAX_CONCURRENT_AGENTS, RunningWork, ScheduledWork, ScheduledWorkKind,
    SchedulerCancellation, SchedulerSnapshot, runnable_agent_ids,
};
