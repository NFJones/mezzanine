//! Agent scheduler ownership operations.
//!
//! This module owns the narrow runtime boundary around queued, running,
//! blocked, cancelled, and concurrency-limited agent work. The scheduler field
//! remains private to `RuntimeAgentComponent`.

use super::{AgentScheduler, ProviderRetryScheduler, Result, RuntimeSessionService, ScheduledWork};
use mez_agent::ProviderRetryPolicy;

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

    /// Applies the configured queued-turn count and estimated-byte budgets.
    pub(crate) fn configure_agent_scheduler_queue_limits(
        &mut self,
        max_queued_turns: usize,
        max_queued_bytes: usize,
    ) -> Result<()> {
        self.agent
            .agent_scheduler
            .set_queue_limits(max_queued_turns, max_queued_bytes)?;
        Ok(())
    }

    /// Applies provider retry policy to subsequent failure observations.
    ///
    /// Active retry generations and already-scheduled timer delays remain
    /// intact; the replacement policy takes effect at the next failure.
    pub(crate) fn configure_provider_retry_policy(&mut self, policy: ProviderRetryPolicy) {
        self.agent.provider_retry_scheduler.set_policy(policy);
    }

    /// Enqueues one validated unit of agent work.
    ///
    /// One accepted inbound prompt turn is exactly one enqueue, so this is also
    /// where the generated-title cadence counts a prompt turn. The tick only
    /// re-arms a due title window: it never queues, fails, or delays the work, and
    /// it runs after the scheduler accepted the work so a rejected enqueue is not
    /// counted as a prompt turn.
    pub(crate) fn enqueue_agent_work(&mut self, work: ScheduledWork) -> Result<()> {
        let conversation_id = work.conversation_id.clone();
        self.agent.agent_scheduler.enqueue(work)?;
        self.agent
            .session_title_tasks
            .note_prompt_turn(&conversation_id);
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
