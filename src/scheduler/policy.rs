//! Read-only scheduling policy helpers.
//!
//! Policy logic lives separately from queue mutation so callers and tests can
//! inspect runnable work without changing scheduler state.

use std::collections::BTreeSet;

use crate::error::{MezError, Result};

use super::types::{AgentScheduler, ScheduledWork, ScheduledWorkKind};

/// Returns the agents with queued work that could start immediately.
pub fn runnable_agent_ids(scheduler: &AgentScheduler) -> BTreeSet<String> {
    scheduler
        .queued_turns()
        .filter(|work| scheduler.can_start(work))
        .map(|work| work.agent_id.clone())
        .collect()
}

/// Runs the validate work operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_work(work: &ScheduledWork) -> Result<()> {
    if work.turn_id.trim().is_empty() || work.agent_id.trim().is_empty() {
        return Err(MezError::invalid_args(
            "scheduled work requires turn and agent identity",
        ));
    }
    if work.kind == ScheduledWorkKind::ShellCapable
        && work
            .pane_id
            .as_deref()
            .is_none_or(|pane_id| pane_id.trim().is_empty())
    {
        return Err(MezError::invalid_args(
            "shell-capable scheduled work requires a pane id",
        ));
    }
    Ok(())
}
