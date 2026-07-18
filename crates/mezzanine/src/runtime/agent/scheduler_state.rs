//! Agent scheduler ownership operations.
//!
//! This module owns the narrow runtime boundary around queued, running,
//! blocked, cancelled, and concurrency-limited agent work. The scheduler field
//! remains private to `RuntimeAgentComponent`.

use super::{AgentScheduler, ProviderRetryScheduler, Result, RuntimeSessionService, ScheduledWork};

impl RuntimeSessionService {
    /// Returns the agent scheduler for read-only diagnostics and prompt context.
    pub(crate) fn agent_scheduler(&self) -> &AgentScheduler {
        &self.agent.agent_scheduler
    }

    /// Returns mutable scheduler access to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn agent_scheduler_mut(&mut self) -> &mut AgentScheduler {
        &mut self.agent.agent_scheduler
    }

    /// Returns mutable provider-retry reducer access to crate-local tests.
    #[cfg(test)]
    pub(crate) fn provider_retry_scheduler_mut(&mut self) -> &mut ProviderRetryScheduler {
        &mut self.agent.provider_retry_scheduler
    }

    /// Applies the configured global agent concurrency limit.
    pub(crate) fn configure_agent_scheduler_limit(
        &mut self,
        max_concurrent_agents: usize,
    ) -> Result<()> {
        self.agent
            .agent_scheduler
            .set_max_concurrent_agents(max_concurrent_agents)?;
        Ok(())
    }

    /// Enqueues one validated unit of agent work.
    pub(crate) fn enqueue_agent_work(&mut self, work: ScheduledWork) -> Result<()> {
        self.agent.agent_scheduler.enqueue(work)?;
        Ok(())
    }

    /// Cancels queued, running, or blocked scheduler work when it exists.
    pub(crate) fn cancel_agent_work(&mut self, turn_id: &str) -> bool {
        self.agent.agent_scheduler.cancel(turn_id).is_ok()
    }

    /// Restores empty work and provider-retry schedulers with default policy.
    pub(crate) fn reset_agent_scheduler(&mut self) {
        self.agent.agent_scheduler = AgentScheduler::with_default_limit();
        self.agent.provider_retry_scheduler = ProviderRetryScheduler::default();
    }
}
